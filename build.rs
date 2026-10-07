fn main() {
    // LadybugDB extensions (vector) are shared libraries that resolve symbols from the host binary.
    println!("cargo:rustc-link-arg=-rdynamic");
}
