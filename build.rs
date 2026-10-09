fn main() {
    if std::env::var("TARGET").as_deref() != Ok("wasm32-unknown-emscripten") {
        return;
    }

    for arg in [
        "-O2",
        // keep the JS-pumped audio entry point reachable from browser glue
        "--export=sion_wasm_pump",
        // miniquad's `mq_js_bundle.js` supplies canvas/WebGL host functions;
        // emscripten must not fail the link on those (or its own runtime)
        "-sERROR_ON_UNDEFINED_SYMBOLS=0",
    ] {
        println!("cargo:rustc-link-arg={arg}");
    }
}
