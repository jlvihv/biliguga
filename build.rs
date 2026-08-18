fn main() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    println!("cargo:rustc-link-arg=-Wl,--export-dynamic-symbol=malloc_conf");
}
