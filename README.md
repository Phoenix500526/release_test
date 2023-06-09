# 基于 key 排队的 mpsc channel
[![mpsc_channel Actions Status](https://github.com/Phoenix500526/mpsc_channel/workflows/build/badge.svg)](https://github.com/Phoenix500526/mpsc_channel/actions)
[![codecov](https://codecov.io/gh/Phoenix500526/mpsc_channel/branch/basic_requirement/graph/badge.svg?token=49MKK81WKF)](https://codecov.io/gh/Phoenix500526/mpsc_channel)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)


## 项目简介

实现一个具备消息冲突检测机制的 mpsc bounded channel。该 channel 中的每个消息都具有一个(或若干个) key，并且每个消息在 send 到 channel 后就处于 Active 状态，直到该消息被 drop 以后才退出该状态。任何新 send 到 channel 中的 msg，若其 key 与任何一个处于 active 状态的 msg 的 key 存在交集，则认为这两个 msg 产生冲突。


## 需求描述

### 功能性需求及相关测试用例

1. 实现 bounded channel 的基本功能
   1. 允许基本的 send/recv 操作 —— 生产者能够产生数据，消费者能够消费产生出来的数据
   2. 满足 mpsc 语义 —— 允许多个生产者生产数据，单个消费者消费数据
   3. 满足 bounded channel 约束 —— 当队列为空时，receiver 所在的线程会阻塞；当队列满时，sender 所在线程会阻塞
   4. 故障检测
      1. 当 receiver 因某种原因被 drop，则 sender 在发送时需要产生一个错误
      2. 当所有的 sender 都因某种原因被 drop，则 receiver 在 recv 的时候会产生一个错误
      3. 如果 receiver 已经进入了 recv 阻塞状态，那么最后一个 sender 被 drop 掉时，需要去唤醒 receiver，否则这个 receiver 将会泄漏

2. 实现冲突相关功能
   1. msg 的冲突检测 —— 如果 send 到 channel 的 msg 存在冲突时，需要将其暂存到 channel 内部
   1. msg 的冲突排序 —— 要求 recv 到的消息满足非冲突顺序，所谓的非冲突顺序的定义是：如果若干个 msg 之间存在某种冲突关系，则将这若干个 msg 划分为一个冲突域，而该冲突域的一个拓扑排序即为该冲突域的非冲突顺序。


### 非功能性需求

1. 以 README 方式写一个 report，简单介绍数据结构的设计与思路
2. 添加指定 check 到 lib.rs 和 main.rs，并通过 cargo clippy 检查
3. 要有丰富的 test cases
4. 以 pr 的形式提交代码


## 数据结构及设计思路
从上述分析来看，可以将开发划分为两个阶段：
1. 完成一个 bounded channel，使其满足 bounded channel 的基本功能需求
2. 完成 Message(若干个 key) 的冲突检测及排序

### bounded channel 的数据结构定义

```rust
/// channel 底层数据结构定义
/// Shared 需要实现 Clone Trait
struct Shared<K, T>
where
    K: Clone + Eq + Hash,
{
    queue: Mutex<VecDeque<Message<K, T>>>,   // channel 队列
    available: Condvar,                      // 条件变量，用以实现队列的空时不取，满时不加
    capacity: AtomicUsize,                   // 队列容量
    senders: AtomicUsize,                    // 记录当前有多少 sender，以便 receiver 在所有 sender 都退出时能够有所感知，不必空等
    receiver: AtomicBool,                    // 判断对端的 receiver 是否已关闭
    active: DashSet<K>,                      // 处于 active 状态 message 的 key
}

/// Sender 需要实现 Clone、Drop Trait
#[derive(Debug)]
pub struct Sender<K, T>
where
    K: Clone + Eq + Hash,
{
    shared: Arc<Shared<K, T>>,
}

/// Receiver 需要实现 Drop Trait
#[derive(Debug)]
pub struct Receiver<K, T>
where
    K: Clone + Eq + Hash,
{
    shared: Arc<Shared<K, T>>,
}


/// message 的数据结构定义
#[derive(Debug)]
pub struct Message<K, T>
where
    K: Clone + Eq + Hash,
{
    keys: Vec<K>,                            // 保存 message 的key
    content: T,                              // message 的内容实体
    shared: Arc<Shared<K, T>>,
}
```

channel 同时还提供了如下几种错误定义：
```rust
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// NoReceiverLeft will generate when a sender try to send a message to a channel without any receiver left.
    #[error("No Receiver Left!")]
    NoReceiverLeft,

    /// NoSenderLeft will generate when a receiver try to receive a message from a channel without any sender left.
    #[error("No Sender Left!")]
    NoSenderLeft,

    /// If another user of this mutex panicked while holding the mutex, then this call will return an error once the mutex is acquired.
    #[error("Poisoned Lock")]
    PoisonedLock,

    /// MessageConfilct will generate when the message which is about to be consumed has at least one key that is same as one of the keys of those active messages.
    #[error("Message Conflict with keys: {0}")]
    MessageConfilct(String),
}
```

### 完成 Message(若干个 key) 的冲突检测及排序 —— ConflictQueue

我们可以将 msg 的相关功能分解为如下若干个小问题：

* 如何表达 msg 的 active 状态及 conflict 状态？是否需要额外的字段来表达？

>  当一个 msg 被 send 到 channel 中就进入到了 active 状态，并且只有在被 drop 以后才会退出 active 状态。因此，我们可以从逻辑上将所有 msg 的 active 转换为成该 msg 是否已经被 drop。
>
> 而当新 send 入 channel 的 msg 的 key 与 active msg 的 key 存在交集时，则它们之间存在冲突。对于非冲突 msg，我们将其保留到 channel 的 fifo queue 中，而冲突的 msg 则将其保留到某个数据结构中。冲突与否可以通过 msg 所处的位置来判断



* 使用什么样的结构来描述它？这个数据结构应当具备什么样的特点？

> 我们可以通过一个类似图结构（Conflict Relationship Graph）来描述冲突。对于所有 active msg，我们都需要为其 key 建立索引 active，这样可以快速判断一个新 send 进来的 msg 是否与其他的 msg 发生了冲突。如果没有冲突，可以将其保存到 fifo queue 中，反之，将其保存到 conflict_buffer 中，conflict_buffer 会以 msg_id 作为索引，(indegree, msg) 为值，其中 indegree 为 msg 节点的冲突数(即该 msg 节点的入度)
>



* 当一个 msg 被 drop 的时候，它应当执行哪些操作？

> 当一个 msg 被 drop 的时候，它需要做如下几件事情：
>
> 1. 获取 conflict_queue 的 mutex lock；
> 2. 根据 dropped msg 的 key 在 conflict_queu.active 中移除对应的 msg id，同时找到与之冲突的 msg_id ，更新它们的入度。如果更新前入度为 1，则直接将该 msg 移入到 fifo_queue 中。
> 2. 必要的情况下唤醒 receiver



为了实现冲突检测及其排序，我实现了一个 ConflictQueue 的数据结构，其数据结构定义如下：

```rust
type MSGID = usize;

pub struct ConflictQueue<K, T>
where
    K: Clone + Eq + Debug + Hash,
{
    active: HashMap<K, HashSet<MSGID>>,	// active 用于保存
    conflict_buffer: HashMap<MSGID, (usize, Message<K, T>)>,
    fifo_queue: VecDeque<Message<K, T>>,
    id: MSGID,
}

impl<K, T> ConflictQueue<K, T>
where
    K: Clone + Eq + Debug + Hash,
{
  // 创建指定容量的 ConflictQueue 对象，其中 conflict_buffer 和 fifo_queue 的 capacity 均为 cap
  pub fn with_capacity(cap: usize) -> Self;
  // 为 msg 生成一个唯一标识符
  pub fn generate_id(&mut self) -> MSGID;
  // 返回当前 conflict_queue 中所有元素的个数，包含了 fifo_queue 和 conflict_buffer 中的元素总数
  pub fn capacity(&self) -> usize;
  // 返回当前 conflict_queue 中非冲突的 msg 个数，仅包含 fifo_queue 中的元素个数
  pub fn len(&self) -> usize;
  pub fn pop_front(&mut self) -> Option<Message<K, T>>;
  pub fn push_back(&mut self, msg: Message<K, T>);
  // 将 msg 添加到 conflict_queue 中，自动根据 msg 是否冲突来选择将 msg 保存到 fifo_queue 或是 conflict_buffer
  pub fn add_msg(&mut self, msg: Message<K, T>);
  // 用于在 drop msg 时，根据 msg 的 key 来 deactivate 相关的 msg，返回 true 表示在 deactivate 过程中有 msg 的冲突状态被解除了(即从 conflict_buffer 移回到了 fifo_queue 当中)
  pub fn deactivate(&mut self, msg_id: MSGID, key: &K) -> bool;
}
```
