//! 负载均衡算法实现
//!
//! 支持四种策略（按 `route.load_balance` 配置）：
//! - `round_robin`        ：轮询，全局计数器取模
//! - `random`             ：随机选取
//! - `least_conn`         ：最小活跃连接数，选中时计数 +1，请求结束 -1（ConnGuard）
//! - `weighted_round_robin`：平滑加权轮询（nginx 算法），权重来自 `upstream_weights`
//!
//! 状态管理：以路由名为 key 维护每路由的均衡状态，配置热更新后若上游列表或权重
//! 发生变化，自动重建该路由的状态（计数清零），保证新旧配置不互相污染。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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
    /// 配置签名：用于检测上游/权重变化，变化时重建状态
    signature: String,
    /// 轮询计数（仅在持锁时访问）
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
            signature: signature_of(route),
            rr_counter: 0,
            wrr_current: vec![0i32; len],
            lc_counts: (0..len).map(|_| Arc::new(AtomicUsize::new(0))).collect(),
        }
    }

    /// 检测配置变化，必要时重建状态
    fn sync(&mut self, route: &RouteConfig) {
        let sig = signature_of(route);
        if self.signature != sig {
            let len = route.upstream.len();
            self.rr_counter = 0;
            self.wrr_current = vec![0i32; len];
            self.lc_counts = (0..len).map(|_| Arc::new(AtomicUsize::new(0))).collect();
            self.signature = sig;
        }
    }
}

/// 计算路由配置签名：上游列表 + 权重 + 策略
fn signature_of(route: &RouteConfig) -> String {
    let weights = route.normalized_weights();
    let weights_str = weights
        .iter()
        .map(|w| w.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{}|{}|{}|{}",
        route.upstream.join(","),
        route.load_balance,
        weights_str,
        route.upstream.len()
    )
}

/// 负载均衡器：以路由名为 key 维护状态，线程安全
#[derive(Default)]
pub struct Balancer {
    states: Mutex<HashMap<String, RouteState>>,
}

impl Balancer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 从给定路由选取一个上游地址，返回 (上游地址, 连接守卫)
    /// 守卫在 drop 时自动递减活跃连接计数（仅 least_conn 策略生效）
    pub fn select(&self, route: &RouteConfig) -> Option<(String, ConnGuard)> {
        if route.upstream.is_empty() {
            return None;
        }

        let key = if route.name.is_empty() {
            "__unnamed__"
        } else {
            route.name.as_str()
        };

        let mut states = self.states.lock();
        let state = states
            .entry(key.to_string())
            .or_insert_with(|| RouteState::new(route));
        state.sync(route);

        let len = route.upstream.len();
        match route.load_balance.as_str() {
            "round_robin" => {
                let idx = state.rr_counter % len;
                state.rr_counter = state.rr_counter.wrapping_add(1);
                Some((route.upstream[idx].clone(), ConnGuard::none()))
            }
            "random" => {
                let idx = rand::thread_rng().gen_range(0..len);
                Some((route.upstream[idx].clone(), ConnGuard::none()))
            }
            "least_conn" => {
                // 选取活跃连接数最小的节点（并列取第一个）
                let min_idx = (0..len)
                    .min_by_key(|&i| state.lc_counts[i].load(Ordering::Relaxed))
                    .unwrap();
                state.lc_counts[min_idx].fetch_add(1, Ordering::Relaxed);
                Some((
                    route.upstream[min_idx].clone(),
                    ConnGuard::some(state.lc_counts[min_idx].clone()),
                ))
            }
            "weighted_round_robin" => {
                let weights = route.normalized_weights();
                let total: i32 = weights.iter().map(|&w| w as i32).sum();
                if total <= 0 {
                    // 兜底：所有权重为 0 已被 normalized_weights 修正为 1，不会进入
                    let idx = state.rr_counter % len;
                    state.rr_counter = state.rr_counter.wrapping_add(1);
                    return Some((route.upstream[idx].clone(), ConnGuard::none()));
                }
                // 平滑加权轮询：每个节点 current_weight += weight，选最大者，再 -= total
                for (i, &w) in weights.iter().enumerate() {
                    state.wrr_current[i] += w as i32;
                }
                let best = (0..len)
                    .max_by_key(|&i| state.wrr_current[i])
                    .unwrap();
                state.wrr_current[best] -= total;
                Some((route.upstream[best].clone(), ConnGuard::none()))
            }
            _ => {
                // 未知策略兜底为轮询
                let idx = state.rr_counter % len;
                state.rr_counter = state.rr_counter.wrapping_add(1);
                Some((route.upstream[idx].clone(), ConnGuard::none()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RouteMatchConfig;

    fn make_route(name: &str, lb: &str, upstreams: &[&str], weights: &[u32]) -> RouteConfig {
        RouteConfig {
            name: name.into(),
            r#match: RouteMatchConfig::default(),
            upstream: upstreams.iter().map(|s| s.to_string()).collect(),
            upstream_weights: weights.to_vec(),
            load_balance: lb.into(),
            ..Default::default()
        }
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
        // 第一次选 a，活跃 +1
        let (p1, g1) = b.select(&route).unwrap();
        assert_eq!(p1, "http://a");
        // 第二次应选 b（a 活跃为 1，b 为 0）
        let (p2, g2) = b.select(&route).unwrap();
        assert_eq!(p2, "http://b");
        // 释放第一个，a 活跃归 0
        drop(g1);
        // 第三次应选 a
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
        // 总权重 4，一轮应选 a 3 次、b 1 次
        let mut counts = HashMap::new();
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
        // 推进计数器
        b.select(&route);
        b.select(&route);
        // 配置变化：新增一个上游
        let route2 = make_route(
            "r1",
            "round_robin",
            &["http://a", "http://b", "http://c"],
            &[],
        );
        // 状态应重建，计数从 0 开始
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
