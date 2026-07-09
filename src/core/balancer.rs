//! 负载均衡算法实现
//!
//! 支持四种策略（按 `route.load_balance` 配置）：
//! - `round_robin`        ：轮询，原子计数器取模
//! - `random`             ：随机选取
//! - `least_conn`         ：最小活跃连接数，选中时计数 +1，请求结束 -1（ConnGuard）
//! - `weighted_round_robin`：平滑加权轮询（nginx 算法），权重来自 `upstream_weights`
//!
//! 状态管理：以路由名为 key 维护每路由的均衡状态（DashMap，无锁读），
//! 配置热更新后若上游列表或权重发生变化，自动重建该路由的状态（计数清零）。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::Mutex;
use rand::Rng;

use crate::config::RouteConfig;

/// 活跃连接守卫：请求结束后自动递减 least_conn 计数
pub struct ConnGuard {
    counter: Option<Arc<AtomicUsize>>,
}

impl ConnGuard {
    fn none() -> Self {
        Self { counter: None }
    }
    fn some(counter: Arc<AtomicUsize>) -> Self {
        Self { counter: Some(counter) }
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        if let Some(c) = self.counter.as_ref() {
            c.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// 单路由的均衡状态
struct RouteState {
    /// 配置签名（u64 hash）：用于检测上游/权重变化，变化时重建状态
    signature: AtomicU64,
    /// 内部可变状态（rr_counter / wrr_current / lc_counts）
    inner: Mutex<RouteStateInner>,
}

struct RouteStateInner {
    /// 轮询计数
    rr_counter: usize,
    /// 平滑加权轮询的当前权重（nginx 算法）
    wrr_current: Vec<i32>,
    /// least_conn 活跃连接计数（Arc 以便 ConnGuard 在锁外递减）
    lc_counts: Vec<Arc<AtomicUsize>>,
}

impl RouteState {
    fn new(route: &RouteConfig) -> Self {
        let len = route.upstream.len();
        Self {
            signature: AtomicU64::new(signature_of(route)),
            inner: Mutex::new(RouteStateInner {
                rr_counter: 0,
                wrr_current: vec![0i32; len],
                lc_counts: (0..len).map(|_| Arc::new(AtomicUsize::new(0))).collect(),
            }),
        }
    }
}

/// 计算路由配置签名（u64 hash）：上游列表 + 权重 + 策略
fn signature_of(route: &RouteConfig) -> u64 {
    let mut hasher = DefaultHasher::new();
    for u in &route.upstream {
        u.hash(&mut hasher);
    }
    route.load_balance.hash(&mut hasher);
    for w in route.normalized_weights() {
        w.hash(&mut hasher);
    }
    route.upstream.len().hash(&mut hasher);
    hasher.finish()
}

/// 负载均衡器：以路由名为 key 维护状态，线程安全（DashMap 无锁读）
#[derive(Default)]
pub struct Balancer {
    states: DashMap<String, Arc<RouteState>>,
}

impl Balancer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 从给定路由选取一个上游地址，返回 (上游地址, 连接守卫)
    /// 守卫在 drop 时自动递减活跃连接计数（仅 least_conn 策略生效）
    pub fn select(&self, route: &Arc<RouteConfig>) -> Option<(String, ConnGuard)> {
        if route.upstream.is_empty() {
            return None;
        }

        let key = if route.name.is_empty() {
            "__unnamed__"
        } else {
            route.name.as_str()
        };

        // DashMap 快路径：存在则直接 get，不存在则插入
        let state = if let Some(entry) = self.states.get(key) {
            entry.clone()
        } else {
            let new_state = Arc::new(RouteState::new(route));
            self.states.entry(key.to_string()).or_insert(new_state).clone()
        };

        // 检测配置变化：签名不匹配时重建内部状态并更新签名
        let current_sig = signature_of(route);
        if state.signature.load(Ordering::Relaxed) != current_sig {
            let mut inner = state.inner.lock();
            let len = route.upstream.len();
            inner.rr_counter = 0;
            inner.wrr_current = vec![0i32; len];
            inner.lc_counts = (0..len).map(|_| Arc::new(AtomicUsize::new(0))).collect();
            state.signature.store(current_sig, Ordering::Relaxed);
        }

        let len = route.upstream.len();
        let mut inner = state.inner.lock();
        match route.load_balance.as_str() {
            "round_robin" => {
                let idx = inner.rr_counter % len;
                inner.rr_counter = inner.rr_counter.wrapping_add(1);
                Some((route.upstream[idx].clone(), ConnGuard::none()))
            }
            "random" => {
                let idx = rand::thread_rng().gen_range(0..len);
                Some((route.upstream[idx].clone(), ConnGuard::none()))
            }
            "least_conn" => {
                // 选取活跃连接数最小的节点；并列时用 rr_counter 轮转打散
                let mut min_idx = 0;
                let mut min_val = inner.lc_counts[0].load(Ordering::Relaxed);
                for i in 1..len {
                    let v = inner.lc_counts[i].load(Ordering::Relaxed);
                    if v < min_val {
                        min_val = v;
                        min_idx = i;
                    }
                }
                let ties: Vec<usize> = (0..len)
                    .filter(|&i| inner.lc_counts[i].load(Ordering::Relaxed) == min_val)
                    .collect();
                if ties.len() > 1 {
                    let pick = inner.rr_counter % ties.len();
                    inner.rr_counter = inner.rr_counter.wrapping_add(1);
                    min_idx = ties[pick];
                }
                inner.lc_counts[min_idx].fetch_add(1, Ordering::Relaxed);
                Some((
                    route.upstream[min_idx].clone(),
                    ConnGuard::some(inner.lc_counts[min_idx].clone()),
                ))
            }
            "weighted_round_robin" => {
                let weights = route.normalized_weights();
                let total: i32 = weights.iter().map(|&w| w as i32).sum();
                if total <= 0 {
                    let idx = inner.rr_counter % len;
                    inner.rr_counter = inner.rr_counter.wrapping_add(1);
                    return Some((route.upstream[idx].clone(), ConnGuard::none()));
                }
                for (i, &w) in weights.iter().enumerate() {
                    inner.wrr_current[i] += w as i32;
                }
                let best = (0..len)
                    .max_by_key(|&i| inner.wrr_current[i])
                    .unwrap();
                inner.wrr_current[best] -= total;
                Some((route.upstream[best].clone(), ConnGuard::none()))
            }
            _ => {
                let idx = inner.rr_counter % len;
                inner.rr_counter = inner.rr_counter.wrapping_add(1);
                Some((route.upstream[idx].clone(), ConnGuard::none()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RouteMatchConfig;

    fn make_route(name: &str, lb: &str, upstreams: &[&str], weights: &[u32]) -> Arc<RouteConfig> {
        Arc::new(RouteConfig {
            name: name.into(),
            r#match: RouteMatchConfig::default(),
            upstream: upstreams.iter().map(|s| s.to_string()).collect(),
            upstream_weights: weights.to_vec(),
            load_balance: lb.into(),
            ..Default::default()
        })
    }

    #[test]
    fn round_robin_cycles_through_upstreams() {
        let b = Balancer::new();
        let route = make_route(
            "r1",
            "round_robin",
            &["http://a", "http://b", "http://c"],
            &[],
        );
        let picks: Vec<String> = (0..6)
            .map(|_| b.select(&route).unwrap().0)
            .collect();
        assert_eq!(picks[0], "http://a");
        assert_eq!(picks[1], "http://b");
        assert_eq!(picks[2], "http://c");
        assert_eq!(picks[3], "http://a");
        assert_eq!(picks[5], "http://c");
    }

    #[test]
    fn random_returns_valid_upstream() {
        let b = Balancer::new();
        let route = make_route("r1", "random", &["http://a", "http://b"], &[]);
        for _ in 0..20 {
            let (pick, _) = b.select(&route).unwrap();
            assert!(pick == "http://a" || pick == "http://b");
        }
    }

    #[test]
    fn least_conn_picks_least_loaded_and_decrements() {
        let b = Balancer::new();
        let route = make_route(
            "r1",
            "least_conn",
            &["http://a", "http://b"],
            &[],
        );
        let (p1, g1) = b.select(&route).unwrap();
        assert_eq!(p1, "http://a");
        let (p2, g2) = b.select(&route).unwrap();
        assert_eq!(p2, "http://b");
        drop(g1);
        let (p3, _) = b.select(&route).unwrap();
        assert_eq!(p3, "http://a");
        drop(g2);
    }

    #[test]
    fn weighted_round_robin_respects_weights() {
        let b = Balancer::new();
        let route = make_route(
            "r1",
            "weighted_round_robin",
            &["http://a", "http://b"],
            &[3, 1],
        );
        let mut counts = std::collections::HashMap::new();
        for _ in 0..4 {
            let (pick, _) = b.select(&route).unwrap();
            *counts.entry(pick).or_insert(0) += 1;
        }
        assert_eq!(counts.get("http://a"), Some(&3));
        assert_eq!(counts.get("http://b"), Some(&1));
    }

    #[test]
    fn config_change_rebuilds_state() {
        let b = Balancer::new();
        let route = make_route(
            "r1",
            "round_robin",
            &["http://a", "http://b"],
            &[],
        );
        b.select(&route);
        b.select(&route);
        let route2 = make_route(
            "r1",
            "round_robin",
            &["http://a", "http://b", "http://c"],
            &[],
        );
        let (p, _) = b.select(&route2).unwrap();
        assert_eq!(p, "http://a");
    }

    #[test]
    fn empty_upstream_returns_none() {
        let b = Balancer::new();
        let route = make_route("r1", "round_robin", &[], &[]);
        assert!(b.select(&route).is_none());
    }
}
