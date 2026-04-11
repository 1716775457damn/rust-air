fn main() {
    let (tx, rx) = std::sync::mpsc::channel();
    rust_air_core::start_monitor(tx);

    println!("监控已启动，等待 8 秒，请在这期间复制一些文字...");
    for i in 1..=8 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let pending: Vec<_> = rx.try_iter().collect();
        println!("第 {}s tick: 收到 {} 条", i, pending.len());
        for item in pending {
            match item {
                rust_air_core::ClipContent::Text { text } =>
                    println!("  TEXT: {:?}", &text[..text.len().min(80)]),
                rust_air_core::ClipContent::Image { width, height, .. } =>
                    println!("  IMAGE: {}x{}", width, height),
            }
        }
    }
    println!("完成");
}
