fn main() {
    let payload = include_bytes!(env!("VSTERM_HOST_PAYLOAD"));
    if let Err(error) =
        arterm::deployment::installer(arterm::deployment::Role::Host, payload)
    {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
