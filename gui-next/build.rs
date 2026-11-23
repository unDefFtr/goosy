fn main() {
    println!("cargo:rerun-if-changed=qml");
    println!("cargo:rerun-if-changed=native/bridge.h");
    cxx_qt_build::CxxQtBuilder::new()
        .include_dir("native")
        .file("src/backend.rs")
        .cpp_file("native/bridge.cpp")
        .qrc("qml/gui_next.qrc")
        .build();
}
