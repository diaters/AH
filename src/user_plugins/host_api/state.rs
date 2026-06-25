use rhai::Engine;

/// 在 Engine 顶层注册的仅是无状态那部分。per-plugin state 通过
/// dispatcher 注入的闭包绑定，在此处不注册。
pub fn register(_engine: &mut Engine) {
    // 占位：currently no global state functions.
}

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {}
}
