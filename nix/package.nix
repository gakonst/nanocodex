{
  lib,
  stdenv,
  stdenvNoCC,
  fetchurl,
  gzip,
  patchelf,
}:

let
  version = "0.6.6";
  releaseUrl = "https://github.com/gakonst/nanocodex/releases/download/v${version}";
  releases = {
    aarch64-darwin = {
      target = "aarch64-apple-darwin";
      nanocodexHash = "sha256-TKxxA/hNu0p1OzfBAJdKBFWjq9CNPm4w14g6USfIJX8=";
      nanocodex2Hash = "sha256-1Qi0Nzhs+qvCiPsjTzJL1v32ETjwf+C69TiMlE6rRuU=";
    };
    x86_64-linux = {
      target = "x86_64-unknown-linux-gnu";
      nanocodexHash = "sha256-UK1F4M7WNnjymci1ZB+tt5eonxHuOOZLM6dG9Ld0h6Y=";
      nanocodex2Hash = "sha256-zSep5KANTNEWdsM5byUt34mGq9twHeqxONUylKqu5hQ=";
    };
  };
  release =
    releases.${stdenvNoCC.hostPlatform.system}
      or (throw "Nanocodex has no release for ${stdenvNoCC.hostPlatform.system}");
  nanocodex = fetchurl {
    url = "${releaseUrl}/nanocodex-${release.target}.gz";
    hash = release.nanocodexHash;
  };
  nanocodex2 = fetchurl {
    url = "${releaseUrl}/nanocodex2-${release.target}.gz";
    hash = release.nanocodex2Hash;
  };
in
stdenvNoCC.mkDerivation {
  pname = "nanocodex";
  inherit version;

  dontUnpack = true;

  nativeBuildInputs = [ gzip ] ++ lib.optionals stdenvNoCC.hostPlatform.isLinux [ patchelf ];
  buildInputs = lib.optionals stdenvNoCC.hostPlatform.isLinux [
    stdenv.cc.libc
    stdenv.cc.cc.lib
  ];

  # Preserve the embedded macOS code signatures. Release binaries are already
  # stripped, so skipping another strip pass is harmless on Linux too.
  dontStrip = true;

  installPhase = ''
    runHook preInstall

    install -d "$out/bin"
    gzip -dc ${nanocodex} > "$out/bin/nanocodex"
    gzip -dc ${nanocodex2} > "$out/bin/nanocodex2"
    chmod 0755 "$out/bin/nanocodex" "$out/bin/nanocodex2"

    ${lib.optionalString stdenvNoCC.hostPlatform.isLinux ''
      for executable in "$out/bin/nanocodex" "$out/bin/nanocodex2"; do
        patchelf \
          --set-interpreter ${stdenv.cc.libc}/lib/ld-linux-x86-64.so.2 \
          --set-rpath ${
            lib.makeLibraryPath [
              stdenv.cc.libc
              stdenv.cc.cc.lib
            ]
          } \
          "$executable"
      done
    ''}

    runHook postInstall
  '';

  meta = {
    description = "Library-first coding agent CLI and TUI";
    homepage = "https://github.com/gakonst/nanocodex";
    license = with lib.licenses; [
      asl20
      mit
    ];
    mainProgram = "nanocodex";
    platforms = builtins.attrNames releases;
    sourceProvenance = with lib.sourceTypes; [ binaryNativeCode ];
  };
}
