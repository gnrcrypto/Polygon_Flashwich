// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import "@uniswap/v3-core/contracts/interfaces/IUniswapV3Pool.sol";
import "@uniswap/v3-core/contracts/interfaces/callback/IUniswapV3FlashCallback.sol";
import "@uniswap/v3-periphery/contracts/libraries/PoolAddress.sol";
import "@uniswap/v3-periphery/contracts/interfaces/ISwapRouter.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/access/Ownable.sol";

interface IFastLaneSender {
    // ⬇️ renamed to sendRawTransaction
    function sendRawTransaction(bytes calldata data, uint256 targetBlock)
        external
        payable
        returns (bytes32);
}

contract FlashLoanArbitrage is IUniswapV3FlashCallback, Ownable {
    ISwapRouter public immutable swapRouter;
    address public immutable WETH;
    address public immutable factory;
    address public fastLaneSender;
    uint256 public maxDelayBlocks = 5;
    uint24 public constant DEFAULT_FEE = 3000;

    struct FlashCallbackData {
        address token0;
        address token1;
        uint256 amount0;
        uint256 amount1;
        uint24 fee;
        address[] path;
        uint256[] amounts;
        address[] routers;
    }

    struct ArbitrageOpportunity {
        address token0;
        address token1;
        uint256 amount0;
        uint256 amount1;
        uint24 fee;
        address[] path;
        uint256[] amounts;
        address[] routers;
    }

    struct FastLaneBundle {
        bytes data;
        uint256 targetBlock;
    }

    event ArbitrageExecuted(
        address indexed token0,
        address indexed token1,
        uint256 amount0,
        uint256 amount1,
        uint256 profit,
        bytes32 bundleHash
    );

    event BundleSubmitted(
        bytes32 bundleHash,
        uint256 targetBlock,
        uint256 gasPrice
    );

    event FlashLoanFailed(
        address pool,
        uint256 amount0,
        uint256 amount1,
        string reason
    );

    constructor(
        address _swapRouter,
        address _weth,
        address _factory
    ) Ownable(msg.sender) {
        require(_swapRouter != address(0), "Invalid swap router");
        require(_weth != address(0), "Invalid WETH");
        require(_factory != address(0), "Invalid factory");
        swapRouter = ISwapRouter(_swapRouter);
        WETH = _weth;
        factory = _factory;
    }

    function setFastLaneSender(address _fastLaneSender) external onlyOwner {
        require(_fastLaneSender != address(0), "Invalid FastLane sender");
        fastLaneSender = _fastLaneSender;
    }

    function setMaxDelayBlocks(uint256 _maxDelayBlocks) external onlyOwner {
        require(
            _maxDelayBlocks > 0 && _maxDelayBlocks <= 10,
            "Invalid block delay"
        );
        maxDelayBlocks = _maxDelayBlocks;
    }

    function executeFlashLoanArbitrage(
        address token0,
        address token1,
        uint256 amount0,
        uint256 amount1,
        uint24 fee,
        address[] calldata path,
        uint256[] calldata amounts,
        address[] calldata routers
    ) external onlyOwner {
        _executeFlashLoanArbitrage(
            token0,
            token1,
            amount0,
            amount1,
            fee,
            path,
            amounts,
            routers
        );
    }

    function _executeFlashLoanArbitrage(
        address token0,
        address token1,
        uint256 amount0,
        uint256 amount1,
        uint24 fee,
        address[] memory path,
        uint256[] memory amounts,
        address[] memory routers
    ) internal {
        PoolAddress.PoolKey memory poolKey = PoolAddress.getPoolKey(
            token0,
            token1,
            fee
        );
        address poolAddress = PoolAddress.computeAddress(factory, poolKey);
        IUniswapV3Pool pool = IUniswapV3Pool(poolAddress);

        bytes memory data = abi.encode(
            FlashCallbackData({
                token0: token0,
                token1: token1,
                amount0: amount0,
                amount1: amount1,
                fee: fee,
                path: path,
                amounts: amounts,
                routers: routers
            })
        );

        pool.flash(address(this), amount0, amount1, data);
    }

    function uniswapV3FlashCallback(
        uint256 fee0,
        uint256 fee1,
        bytes calldata data
    ) external override {
        FlashCallbackData memory decoded = abi.decode(
            data,
            (FlashCallbackData)
        );

        PoolAddress.PoolKey memory poolKey = PoolAddress.getPoolKey(
            decoded.token0,
            decoded.token1,
            decoded.fee
        );
        address poolAddress = PoolAddress.computeAddress(factory, poolKey);
        require(msg.sender == poolAddress, "Callback not from expected pool");

        uint256 startBalance0 = IERC20(decoded.token0).balanceOf(address(this));
        uint256 startBalance1 = IERC20(decoded.token1).balanceOf(address(this));

        try
            this.executeArbitrageInternal(
                decoded.path,
                decoded.amounts,
                decoded.routers
            )
        {
            // Success - continue with repayment
        } catch Error(string memory reason) {
            emit FlashLoanFailed(
                msg.sender,
                decoded.amount0,
                decoded.amount1,
                reason
            );
            revert(reason);
        }

        uint256 finalBalance0 = IERC20(decoded.token0).balanceOf(address(this));
        uint256 finalBalance1 = IERC20(decoded.token1).balanceOf(address(this));

        // Verify and repay flash loan
        require(finalBalance0 >= decoded.amount0 + fee0, "Insufficient token0");
        require(finalBalance1 >= decoded.amount1 + fee1, "Insufficient token1");

        // Repay flash loan
        IERC20(decoded.token0).transfer(msg.sender, decoded.amount0 + fee0);
        IERC20(decoded.token1).transfer(msg.sender, decoded.amount1 + fee1);

        // Calculate and transfer profit
        uint256 profit0 = finalBalance0 - (decoded.amount0 + fee0);
        uint256 profit1 = finalBalance1 - (decoded.amount1 + fee1);

        if (profit0 > 0) {
            IERC20(decoded.token0).transfer(owner(), profit0);
        }
        if (profit1 > 0) {
            IERC20(decoded.token1).transfer(owner(), profit1);
        }

        emit ArbitrageExecuted(
            decoded.token0,
            decoded.token1,
            decoded.amount0,
            decoded.amount1,
            profit0 + profit1,
            blockhash(block.number - 1)
        );
    }

    function executeArbitrageInternal(
        address[] memory path,
        uint256[] memory amounts,
        address[] memory routers
    ) external {
        require(msg.sender == address(this), "Only self-call");
        _executeArbitrage(path, amounts, routers);
    }

    function _executeArbitrage(
        address[] memory path,
        uint256[] memory amounts,
        address[] memory routers
    ) internal {
        require(path.length >= 2, "Invalid path");
        require(path.length == amounts.length + 1, "Invalid amounts");
        require(path.length == routers.length + 1, "Invalid routers");

        for (uint256 i = 0; i < path.length - 1; i++) {
            address tokenIn = path[i];
            address tokenOut = path[i + 1];
            uint256 amountIn = amounts[i];
            address router = routers[i];

            // Reset and approve token spending
            IERC20(tokenIn).approve(router, 0);
            IERC20(tokenIn).approve(router, amountIn);

            ISwapRouter(router).exactInputSingle(
                ISwapRouter.ExactInputSingleParams({
                    tokenIn: tokenIn,
                    tokenOut: tokenOut,
                    fee: DEFAULT_FEE,
                    recipient: address(this),
                    deadline: block.timestamp + 120,
                    amountIn: amountIn,
                    amountOutMinimum: 0,
                    sqrtPriceLimitX96: 0
                })
            );
        }
    }

    function executeArbitrageWithFastLane(
        ArbitrageOpportunity memory opportunity,
        uint256 targetBlock
    ) external payable onlyOwner returns (bytes32) {
        require(targetBlock > block.number, "Invalid block number");
        require(targetBlock <= block.number + maxDelayBlocks, "Block too far");
        require(fastLaneSender != address(0), "FastLane sender not set");

        FastLaneBundle memory bundle = prepareFastLaneBundle(
            opportunity,
            targetBlock
        );

        // ⬇️ call the renamed method
        bytes32 bundleHash = IFastLaneSender(fastLaneSender).sendRawTransaction{
            value: msg.value
        }(bundle.data, bundle.targetBlock);

        emit BundleSubmitted(bundleHash, targetBlock, tx.gasprice);
        return bundleHash;
    }

    function prepareFastLaneBundle(
        ArbitrageOpportunity memory opportunity,
        uint256 targetBlock
    ) internal pure returns (FastLaneBundle memory) {
        bytes memory callData = abi.encodeWithSelector(
            FlashLoanArbitrage.executeFlashLoanArbitrage.selector,
            opportunity.token0,
            opportunity.token1,
            opportunity.amount0,
            opportunity.amount1,
            opportunity.fee,
            opportunity.path,
            opportunity.amounts,
            opportunity.routers
        );

        return FastLaneBundle({data: callData, targetBlock: targetBlock});
    }

    function withdrawToken(address token, uint256 amount) external onlyOwner {
        require(token != address(0), "Invalid token");
        IERC20(token).transfer(owner(), amount);
    }

    receive() external payable {}
}
