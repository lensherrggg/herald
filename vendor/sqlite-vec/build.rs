// The published 0.1.10-alpha.4 crate references sqlite-vec-diskann.c but does
// not ship it. Herald only needs the flat (vec0) index, so disable diskann.
fn main() {
    cc::Build::new()
        .file("sqlite-vec.c")
        .define("SQLITE_CORE", None)
        .define("SQLITE_VEC_ENABLE_DISKANN", "0")
        .define("SQLITE_VEC_ENABLE_RESCORE", "0")
        .compile("sqlite_vec0");
}
