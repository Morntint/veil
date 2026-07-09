//! 线程安全工具：基于 OnceLock 的全局只读变量封装

use std::sync::OnceLock;

/// 全局只读变量：线程安全地存储一次性初始化的全局值
pub struct Global<T: 'static + Send + Sync>(OnceLock<T>);

impl<T: 'static + Send + Sync> Global<T> {
    /// 创建空的全局变量容器
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    /// 若未初始化则用 `f` 初始化，并返回引用
    pub fn get_or_init<F: FnOnce() -> T>(&self, f: F) -> &T {
        self.0.get_or_init(f)
    }

    /// 返回已初始化的引用，未初始化返回 None
    pub fn get(&self) -> Option<&T> {
        self.0.get()
    }
}

impl<T: 'static + Send + Sync> Default for Global<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_or_init_is_idempotent() {
        let g = Global::<u32>::new();
        let a = g.get_or_init(|| 42);
        let b = g.get_or_init(|| 99);
        assert_eq!(*a, 42);
        assert_eq!(*b, 42);
    }
}
