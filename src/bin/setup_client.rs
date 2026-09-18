fn main() {
    let payload = include_bytes!(env!("VSTERM_CLIENT_PAYLOAD"));
    if let Err(error) =
        arterm::deployment::installer(arterm::deployment::Role::Client, payload)
    {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
