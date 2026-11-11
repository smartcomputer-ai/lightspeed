use std::{
    io::{self, Write},
    process::{Command, Stdio},
    time::Duration,
};
fn main() {
    match std::env::args().nth(1).expect("fixture mode").as_str() {
        "echo" => {
            io::copy(&mut io::stdin(), &mut io::stdout()).unwrap();
        }
        "argv" => {
            for arg in std::env::args().skip(2) {
                println!(
                    "{}",
                    arg.as_bytes()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                );
            }
        }
        "sleep" => {
            println!("PID:{}\nREADY", std::process::id());
            io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
        "tree-exit" | "tree-running" => {
            let child = Command::new(std::env::current_exe().unwrap())
                .arg("sleep")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            println!("CHILD:{}", child.id());
            io::stdout().flush().unwrap();
            if std::env::args().nth(1).unwrap() == "tree-running" {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        "interactive" => {
            println!("READY");
            io::stdout().flush().unwrap();
            loop {
                let mut line = String::new();
                assert_ne!(io::stdin().read_line(&mut line).unwrap(), 0);
                println!("LINE:{}", line.trim());
                io::stdout().flush().unwrap();
            }
        }
        "exit" => {
            println!("completed ü 空");
        }
        other => panic!("unknown mode {other}"),
    }
}
