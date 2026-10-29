//! Exercise exactly the native dispatcher, without creating a billable order.
use std::io::Read;
#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(
        args.len(),
        3,
        "request <source-slug> <endpoint-path>, JSON on stdin"
    );
    let mut body = Vec::new();
    std::io::stdin().take(32769).read_to_end(&mut body).unwrap();
    match nl_benefits::answer(&args[1], "POST", &args[2], &body).await {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
        Err(e) => {
            eprintln!("{} {}: {}", e.status, e.code, e.message);
            std::process::exit(1);
        }
    }
}
