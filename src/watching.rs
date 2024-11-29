use anyhow::{anyhow, Result};
use cfmms::{
    checkpoint::sync_pools_from_checkpoint,
    dex::{Dex, DexVariant},
    pool::Pool,
    sync::sync_pairs,
};
use dashmap::DashMap;
use ethers::{
    abi,
    providers::{Middleware, Provider, Ws},
    types::{Address, BlockNumber, Diff, TraceType, Transaction, H160, H256, U256, U64},
    utils::keccak256,
};
use log::info;
use std::{path::Path, str::FromStr, sync::Arc};
use tokio::{
    sync::broadcast::{self, Sender},
    task::JoinSet,
};
// tokio_stream::StreamExt 提供了更完整的流处理功能  ethers::providers::StreamExt 是 ethers 特定的流扩展
use tokio_stream::StreamExt;

use crate::utils::calculate_next_block_base_fee;
// #[derive(Default, Debug, Clone)] 是 Rust 的属性宏，用于自动实现特定的 trait。
// 实现 Debug trait，允许使用 {:?} 格式化打印
// 实现 Clone trait，允许创建值的深拷贝
// 实现 Default trait，提供默认值
#[derive(Default, Debug, Clone)]
pub struct NewBlock {
    pub number: U64,
    pub gas_used: U256,
    pub gas_limit: U256,
    pub base_fee_per_gas: U256,
    pub timestamp: U256,
}
#[derive(Debug, Clone)]
pub enum Event {
    NewBlock(NewBlock),
    Transaction(Transaction),
}
// struct StateDiff {
//     storage_changes: Map<Address, StorageChanges>,  // 存储变化
//     balance_changes: Map<Address, BalanceChange>,   // 余额变化
//     nonce_changes: Map<Address, NonceChange>,       // nonce变化
//     // ... 其他状态变化
// }
// subscribe_pending_txs  注意这里是拿到的pending的交易 然后使用trace_call直接去模拟执行这些pending的交易 查看状态的改变
async fn trace_state_diff(
    provider: Arc<Provider<Ws>>,
    tx: &Transaction,
    block_number: U64,
    pools: &DashMap<H160, Pool>,
    target_address: Address,
) -> Result<()> {
    info!(
        "Tx #{} received. Checking if it touches: {}",
        tx.hash, target_address
    );
    // trace_call 是以太坊的调试/跟踪功能，用于模拟执行交易并获取详细信息
    // 模拟执行交易，但不实际改变链上状态 可以获取执行过程中的所有状态变化 可以看到存储变化、余额变化等 对于调试和监控很有用
    // BTreeMap 的特点：
    // 1. 有序的 - 按键（地址）排序
    // 2. 基于 B 树实现
    // 3. 内存占用可能比 HashMap 小
    // 4. 适合需要按顺序访问的场景
    let state_diff = provider
        .trace_call(
            tx,                                    // 要模拟执行的交易
            vec![TraceType::StateDiff],            // 指定要跟踪的类型：状态变化
            Some(BlockNumber::from(block_number)), // 在哪个区块执行跟踪
        )
        .await?
        // state_diff 包含交易执行前后的状态差异
        .state_diff
        .ok_or(anyhow!("state diff does not exist"))?
        .0;
    let touched_pools: Vec<Pool> = state_diff
        .keys()
        // pools.get(addr) 从 DashMap 中查找地址对应的池子
        .filter_map(|addr| {
            // 因为pools不拿所有权只是拿的引用 所以这里返回的是ref  需要* 解引用
            // DashMap<Address, Pool> 中的数据是共享的
            // 如果直接使用引用，可能会遇到生命周期问题
            pools.get(addr).map(
                // 如果找到了，对 Ref<Pool> 进行操作
                |p| 
                 // *p.value() 解引用获取 Pool 值
                 // .clone() 克隆这个 Pool
                 // 如果不克隆，我们会一直持有 DashMap 的读锁
                // 这可能导致其他需要写入的操作被阻塞
                // 克隆后可以立即释放锁
                // 克隆后，我们有了自己的数据副本
                // 不用担心其他线程同时修改数据
                // 可以安全地进行后续操作
                (*p.value()).clone(), // ✅ 拥有数据的所有权
                // 这里锁已经释放
            )
        })
        .filter(|p| match p {
               // 检查是否是我们关注的代币池
            Pool::UniswapV2(pool) => vec![pool.token_a, pool.token_b].contains(&target_address),
            Pool::UniswapV3(pool) => vec![pool.token_a, pool.token_b].contains(&target_address),
        })
        .collect();
     // 如果没有影响到任何相关池子，直接返回 这里是与目标地址相关的池子
    if touched_pools.is_empty() {
        return Ok(());
    }
    // 获取目标代币地址的状态变化
    let target_storage = &state_diff
        .get(&target_address)
        .ok_or(anyhow!("no target storage"))?
        .storage; // 获取存储变化信息
    // 对每个受影响的池子进行检查
    for pool in &touched_pools {
        // 计算存储槽
        // keccak256(abi::encode(...)) 是计算存储位置的标准方式
        // 在 ERC20 合约中，通常使用这种方式存储余额映射
        let slot = H256::from(keccak256(abi::encode(&[
            abi::Token::Address(pool.address()),
            abi::Token::Uint(U256::from(3)),
        ])));
       // 将存储值转换为数字
        if let Some(Diff::Changed(c)) = target_storage.get(&slot) {
            let from = U256::from(c.from.to_fixed_bytes());
            let to = U256::from(c.to.to_fixed_bytes());

            if to > from {
                // 如果余额增加，说明这个池子收到了目标代币
                // 这通常意味着有人用目标代币换取了其他代币
                // if to > from, the balance of pool's <target_token> has increased
                // thus, the transaction was a call to swap: <target_token> -> token
                info!(
                    "(Tx #{}) Balance change: {} -> {} @ Pool {}",
                    tx.hash,
                    from,
                    to,
                    pool.address()
                );
            }
        }
    }
    Ok(())
}
pub async fn watching_pool(target_address: Address) -> Result<()> {
    let ws_url = std::env::var("WSS_URL").unwrap();
    let provider = Provider::<Ws>::connect(ws_url).await?;
    let provider = Arc::new(provider);
    // 使用amms同步uniswap V3上的所有的池子
    // 用来保存已经同步过的池子信息
    let checkout_point_path = ".amms-checkpoint.json";
    // Path::new() 创建一个新的路径对象
    // 返回一个布尔值：true 表示文件存在，false 表示文件不存在
    // 这用于判断是否需要从头开始同步，还是可以从上次的检查点继续同步
    let checkpoint_exists = Path::new(checkout_point_path).exists();
    // DashMap 是一个线程安全的哈希表，类似于 HashMap 但是可以在多线程环境下安全使用
    let pools: DashMap<Address, Pool> = DashMap::new();
    let dexes_data = [(
        "0x1F98431c8aD98523631AE4a59f267346ea31F984",
        DexVariant::UniswapV3,
        12369621u64,
    )];
    // into_iter() 会消耗原始集合，获得其所有权
    // 与之相对的是:
    // iter() - 借用集合的不可变引用
    // iter_mut() - 借用集合的可变引用
    // Vec<_> 中的 _ 是类型推断
    // Rust 会根据上下文推断出具体类型
    let dexes: Vec<_> = dexes_data
        .into_iter()
        // map() 用于将一个集合中的每个元素转换成另一种类型 map中要注意 要返回一个值   注意分号
        .map(|(address, variant, number)| {
            Dex::new(H160::from_str(address).unwrap(), variant, number, Some(300))
        })
        // collect() 用于将迭代器转换回一个集合
        // 它可以创建多种集合类型，如 Vec, HashSet 等
        .collect();
    let pools_vec = if checkpoint_exists {
        let (_, pool_vec) =
        // 从之前保存的检查点文件中恢复池子信息 不需要重新扫描区块链历史 返回DEX列表和对应的池子列表
            sync_pools_from_checkpoint(checkout_point_path, 100000, provider.clone()).await?;
        pool_vec
    } else {
        // 直接从区块链上同步最新的池子信息 扫描历史区块获取所有创建的池子 可以选择保存检查点供下次使用 返回同步到的所有池子列表
        sync_pairs(dexes.clone(), provider.clone(), Some(checkout_point_path)).await?
    };
    for pool in pools_vec {
        pools.insert(pool.address(), pool);
    }
    info!("Uniswap V3 pools synced: {}", pools.len());
    // stream data
    // 创建一个广播通道，缓冲区大小为512 // sender: 可以向多个接收者发送消息 // receiver: 接收消息
    let (event_sender, _receiver): (Sender<Event>, _) = broadcast::channel(512);
    // JoinSet 是 tokio 提供的一个任务集合管理器： 可以添加多个异步任务 等待任务完成 动态管理任务生命周期
    let mut set = JoinSet::new();
    /////////////////////////////////////
    //////////////订阅区块信息///////////
    ////////////////////////////////////
    {
        // 创建新的作用域
        // 这里的变量在作用域结束后会被释放
        // 需要 clone 的原因：
        // async move 会获取变量的所有权
        // 原始的 provider 和 event_sender 还需要在其他地方使用
        // Arc/Clone 允许多个所有者共享数据
        let provider = provider.clone();
        let event_sender = event_sender.clone();
        // Stream 可以看作是异步版本的 Iterator！
        set.spawn(async move {
            let stream = provider.subscribe_blocks().await.unwrap();
            // 有block Number 返回NewBlock 否则none
            let mut stream = stream.filter_map(|block| match block.number {
                Some(number) => Some(NewBlock {
                    number,
                    gas_used: block.gas_used,
                    gas_limit: block.gas_limit,
                    base_fee_per_gas: block.base_fee_per_gas.unwrap_or_default(),
                    timestamp: block.timestamp,
                }),
                None => None,
            });
            // 这是一个循环模式匹配
            // stream.next().await 返回 Option<NewBlock>
            // while let 会一直循环直到匹配失败（即收到 None）
            while let Some(block) = stream.next().await {
                // block 是解构出来的 NewBlock 类型值
                // 只有当 stream.next() 返回 Some 时才会执行这个代码块
                match event_sender.send(Event::NewBlock(block)) {
                    // 在 Rust 中，match 语句的分支如果是代码块（用 {} 包裹），最后一个分支后不需要逗号
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        });
    }
    {
        // provider
        // 克隆是因为这些变量要在新的异步任务中使用
        let provider = provider.clone();
        let event_sender = event_sender.clone();
        set.spawn(async move {
            // 订阅链上的pending交易池
            // 返回一个包含交易哈希的流
            let stream = provider.subscribe_pending_txs().await.unwrap();
            // transactions_unordered(256):
            // - 并行获取最多256笔交易的详细信息
            // - 返回的交易可能是无序的，但效率更高
            // fuse():
            // - 使流在遇到第一个 None 后就永久关闭
            // - 防止在流断开后继续尝试获取数据
            let mut stream = stream.transactions_unordered(256).fuse();
            // // next() 方法会改变流的内部状态，移动到下一个 所以需要mut
            while let Some(result) = stream.next().await {
                match result {
                    // 第一层match处理交易获取的结果
                    // 第二层match处理事件发送的结果
                    Ok(tx) => match event_sender.send(Event::Transaction(tx)) {
                        Ok(_) => {}
                        Err(_) => {}
                    },
                    Err(_) => {}
                }
            }
        });
    }
    {
        // 订阅事件发送器，创建一个新的接收器
        let mut event_receiver = event_sender.subscribe();
        // 创建一个新的异步任务
        set.spawn(async move {
            // 创建一个默认的新区块状态跟踪器
            let mut new_block = NewBlock::default();

            loop {
                // 等待接收事件
                // 第一层匹配：从 recv() 获取 Result<Event>
                match event_receiver.recv().await {
                    // 成功接收到事件
                    // event 是 Event 枚举类型
                    Ok(event) => match event {
                        // 处理新区块事件
                        // 第二层匹配：匹配 Event 枚举的具体变体
                        // 解构 NewBlock 变体，提取其中的 NewBlock 数据到 block 变量
                        Event::NewBlock(block) => {
                            // 更新当前区块信息
                            // block 现在是 NewBlock 类
                            new_block = block;
                            info!("{:?}", new_block);
                        }
                        // 处理交易事件
                        // 解构 Transaction 变体，提取其中的 Transaction 数据到 tx 变量
                        Event::Transaction(tx) => {
                            // 确保已经有了区块信息  // tx 现在是 Transaction 类型
                            if new_block.number != U64::zero() {
                                // 计算下一个区块的基础费用
                                let next_base_fee = calculate_next_block_base_fee(
                                    new_block.gas_used,
                                    new_block.gas_limit,
                                    new_block.base_fee_per_gas,
                                );
                                // 检查交易的最大费用是否高于下一个区块的基础费用
                                // - EIP-1559 定价机制
                                //   - 基础费用是动态的
                                //   - 矿工倾向于选择高于基础费用的交易
                                //   - 低于基础费用的交易难以被打包
                                if tx.max_fee_per_gas.unwrap_or_default()
                                    > U256::from(next_base_fee)
                                {
                                    // 如果条件满足，追踪状态变化
                                    match trace_state_diff(
                                        provider.clone(),
                                        &tx,
                                        new_block.number,
                                        &pools,
                                        target_address.clone(),
                                    )
                                    .await
                                    {
                                        Ok(_) => {}
                                        Err(_) => {}
                                    }
                                }
                            }
                        }
                    },
                    Err(_) => {}
                }
            }
        });
    }
    // 回顾之前我们spawned了三个任务：
    // 任务1: 监控新区块
    // 任务2: 监控pending交易
    // 任务3: 处理事件
    // join_next() 会等待任意一个任务完成并返回其结果
    // 如果所有任务都完成，返回 None
    while let Some(res) = set.join_next().await {
        info!("{:?}", res);
    }

    Ok(())
}
/* 引用与clone
 使用 & （引用）的场景：
 1. 临时使用数据，不需要改变所有权
 2. 方法要求传入引用
 3. 数据量较大且只需要读取
 使用 clone 的场景：
 1. 需要数据的独立副本来修改
 2. 在多线程环境下共享数据
 3. 生命周期超出原始数据
 决策流程：
  1. 数据是否需要修改？
  2. 数据的生命周期要求？
  3. 是否在多线程环境？
  4. 性能要求？ 优先考虑 &
 */
