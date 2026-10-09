use std::{
    env, fs,
    io::{Read, Write},
    process, thread,
    time::Duration,
};

#[link(name = "kernel32")]
extern "system" {
    fn GetConsoleWindow() -> *mut std::ffi::c_void;
}

fn main() {
    let root = env::current_exe().unwrap().parent().unwrap().to_owned();
    let mode = fs::read_to_string(root.join("mode")).unwrap();
    let args: Vec<String> = env::args().skip(1).collect();
    let ready = root.join("ready");
    let log = |message: &str| {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("calls"))
            .unwrap();
        writeln!(file, "{message}").unwrap();
    };
    if args == ["user", "show", "--json"] {
        log("status");
        match mode.as_str() {
            "malformed" => println!(r#"{{"status":"Login token expired""#),
            "unknown" => println!(r#"{{"status":"service unavailable"}}"#),
            "network" => {
                eprintln!("network unavailable");
                process::exit(1);
            }
            "truncated" => {
                println!(
                    r#"{{"status":"Login token expired","padding":"{}"}}"#,
                    "x".repeat(4 * 1024 * 1024)
                );
            }
            "status_hang" => thread::sleep(Duration::from_secs(90)),
            _ if mode == "valid" || ready.exists() => {
                println!(r#"{{"status":"Logged in as fixture (Microsoft)"}}"#);
            }
            "missing" => {
                eprintln!("Not logged in");
                process::exit(1);
            }
            _ => println!(r#"{{"status":"Login token expired"}}"#),
        }
        return;
    }
    if args == ["user", "login", "--github"] {
        log(if unsafe { GetConsoleWindow() }.is_null() {
            "login-hidden"
        } else {
            "login-console"
        });
        println!("SECRET_OAUTH_URL=https://fixture.invalid/TOKEN");
        eprintln!("SECRET_OAUTH_TOKEN");
        match mode.as_str() {
            "login_fail" | "concurrent_fail" => {
                thread::sleep(Duration::from_millis(500));
                process::exit(7);
            }
            "login_hang" => thread::sleep(Duration::from_secs(90)),
            "false_success" => return,
            _ => {}
        }
        let mut stdin = Vec::new();
        std::io::stdin().read_to_end(&mut stdin).unwrap();
        assert!(stdin.is_empty());
        thread::sleep(Duration::from_millis(500));
        fs::write(ready, b"ready").unwrap();
        return;
    }
    if args.first().map(String::as_str) == Some("list") {
        log("list");
        if mode == "discovery_expiry" && !root.join("discovered").exists() {
            fs::write(root.join("discovered"), b"1").unwrap();
            let _ = fs::remove_file(ready);
            eprintln!("Login token expired");
            process::exit(1);
        }
        println!(r#"{{"tunnels":[]}}"#);
        return;
    }
    if args.first().map(String::as_str) == Some("connect") {
        log("connect");
        if mode == "forward_success" {
            let address = fs::read_to_string(root.join("address")).unwrap();
            println!("Forwarding from {address} to host port 31545");
            std::io::stdout().flush().unwrap();
            thread::sleep(Duration::from_secs(90));
            return;
        }
        if mode == "forward_expiry" || mode == "continued_failure" {
            let _ = fs::remove_file(ready);
            eprintln!("Login token expired");
        } else if mode == "unauthorized" {
            eprintln!("Unauthorized: HTTP 403");
        } else if mode == "unauthorized_expiry" {
            if !root.join("denied").exists() {
                let _ = fs::remove_file(ready);
                fs::write(root.join("denied"), b"1").unwrap();
            }
            eprintln!("Unauthorized: HTTP 403");
        } else {
            eprintln!("fixture connection unavailable");
        }
        process::exit(1);
    }
    println!(r#"{{"ports":[]}}"#);
}
