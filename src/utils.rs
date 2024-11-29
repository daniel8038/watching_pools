use ethers::types::U256;
use rand::Rng;

// 计算下个区块基础费用
pub fn calculate_next_block_base_fee(
    gas_used: U256,
    gas_limit: U256,
    base_fee_per_gas: U256,
) -> U256 {
    let gas_used = gas_used;
    // 确定目标燃料使用量 目标是区块燃料限制（gas limit）的 50% 这是以太坊想要维持的理想区块使用率
    let mut target_gas_used = gas_limit / 2;
    target_gas_used = if target_gas_used == U256::zero() {
        U256::one()
    } else {
        target_gas_used
    };
    // 比较实际使用量和目标使用量
    let new_base_fee = {
        // 如果实际使用量 > 目标使用量（区块拥挤）
        if gas_used > target_gas_used {
            // 增加基础费用
            // 增加量 = 当前基础费用 * (实际使用量 - 目标使用量) / 目标使用量 / 8
            base_fee_per_gas
                + base_fee_per_gas * (gas_used - target_gas_used)
                    / target_gas_used
                    / U256::from(8u64)
        } else {
            // 如果实际使用量 < 目标使用量（区块未充分使用）
            // 减少基础费用
            // 减少量 = 当前基础费用 * (目标使用量 - 实际使用量) / 目标使用量 / 8
            base_fee_per_gas
                - base_fee_per_gas * (gas_used - target_gas_used)
                    / target_gas_used
                    / U256::from(8u64)
        }
    };
    let seed = rand::thread_rng().gen_range(0..9);
    new_base_fee + U256::from(seed)
}
