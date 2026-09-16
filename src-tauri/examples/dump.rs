//! 调试工具：让与主程序完全相同的 Engine 代码直接读取真实数据并打印快照
//! 用法：cargo run --example dump
#[path = "../src/metrics.rs"]
mod metrics;
#[path = "../src/liveio.rs"]
mod liveio;

use metrics::Engine;

fn main() {
    let mut e = Engine::new();
    let mut li = liveio::LiveIo::new();
    println!("数据源: {}", e.data_source_label());
    for i in 0..8 {
        let calls = e.poll();
        li.observe(&calls);
        li.set_inflight(e.call_in_flight());
        let live = li.measure(e.snapshot().now_ms);
        let s = e.snapshot();
        println!(
            "[轮 {}] 当前 {:.1} t/s | 今日均值 {:.1} t/s | 今日总 {} tok (出 {}/入 {}/缓存写 {}/缓存读 {}) | 调用 {} 次 / {} 会话 | live可用={} streaming={} tps={:.1}",
            i,
            s.current_tps,
            s.avg_tps,
            s.total_tokens,
            s.output_tokens,
            s.input_tokens,
            s.cache_creation_tokens,
            s.cache_read_tokens,
            s.calls_today,
            s.sessions_today,
            live.available,
            live.streaming,
            live.tps,
        );
        std::thread::sleep(std::time::Duration::from_millis(600));
    }
}
