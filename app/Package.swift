// swift-tools-version:5.9
import PackageDescription

// The Rust engine is linked as a static library (libnexus_ffi.a) produced by
// `cargo build --release -p nexus-ffi`. Build this package from the `app/`
// directory so the relative `-L` path resolves to ../engine/target/release.
let package = Package(
    name: "NexusView",
    platforms: [.macOS(.v13)],
    targets: [
        // C module exposing the engine's hand-written header to Swift.
        .systemLibrary(name: "CNexusEngine", path: "Sources/CNexusEngine"),

        // The native AppKit application.
        .executableTarget(
            name: "NexusView",
            dependencies: ["CNexusEngine"],
            linkerSettings: [
                // Link the engine statically by naming the archive directly, so
                // the produced binary is self-contained (no dev-path .dylib
                // dependency — `ld` would otherwise prefer the .dylib). The path
                // is relative to the package dir, so build from `app/`.
                .unsafeFlags([
                    "-Xlinker", "../engine/target/release/libnexus_ffi.a",
                ]),
                // Frameworks/libs the statically-linked Rust std may reference.
                .linkedFramework("CoreFoundation"),
                .linkedFramework("Security"),
                .linkedLibrary("c++"),
            ]
        ),
    ]
)
