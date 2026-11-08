fn main() {
    if let Err(error) = codereader::cli::run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
