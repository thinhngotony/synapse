{
  lib,
  stdenvNoCC,
  buildNpmPackage,
  fetchurl,
  bun,
  makeWrapper,
}:
let
  version = "17.2.2";

  # The upstream package is published to npm only (no source repo tarball), and
  # dist/cli.js is a pre-bundled Bun script with a `#!/usr/bin/env bun` shebang.
  # So this is a fetch + resolve-deps + wrap job, not a compile.
  src = fetchurl {
    url = "https://registry.npmjs.org/@oh-my-pi/pi-coding-agent/-/pi-coding-agent-${version}.tgz";
    hash = "sha256-5Vw1IYgOiOO7TxRbx1q02YPiQPkEXX/OaBDwrTuMs/Y=";
  };

  # dist/cli.js resolves these at runtime rather than inlining them. The
  # per-platform natives package is selected by npm from pi-natives'
  # optionalDependencies via os/cpu fields, so the lockfile carries all five and
  # npm installs only the one matching the build platform.
  nativesFor = {
    "x86_64-linux" = "@oh-my-pi/pi-natives-linux-x64";
    "aarch64-linux" = "@oh-my-pi/pi-natives-linux-arm64";
    "x86_64-darwin" = "@oh-my-pi/pi-natives-darwin-x64";
    "aarch64-darwin" = "@oh-my-pi/pi-natives-darwin-arm64";
  };

  inherit (stdenvNoCC.hostPlatform) system;
in
buildNpmPackage (finalAttrs: {
  pname = "omp";
  inherit version src;

  # package.json/package-lock.json pin @oh-my-pi/pi-coding-agent as a dependency
  # so npm resolves the full runtime tree (the published package ships neither).
  postPatch = ''
    cp ${./omp/package.json} package.json
    cp ${./omp/package-lock.json} package-lock.json
  '';

  npmDepsHash = "sha256-qIIxPS2Gt8DcwwzYQF+Xv3ywqocAcCgqRizot4oEmz8=";

  # No build script, and lifecycle scripts in the dep tree must not run.
  dontNpmBuild = true;
  npmFlags = [ "--ignore-scripts" ];

  nativeBuildInputs = [ makeWrapper ];

  installPhase = ''
    runHook preInstall

    mkdir -p $out/lib/omp
    cp -r dist node_modules package.json $out/lib/omp/

    makeWrapper ${lib.getExe bun} $out/bin/omp \
      --add-flags "$out/lib/omp/dist/cli.js"

    runHook postInstall
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    test -e "$out/lib/omp/node_modules/${nativesFor.${system}}" \
      || { echo "missing platform natives for ${system}"; exit 1; }
    export HOME=$(mktemp -d)
    $out/bin/omp --version
    runHook postInstallCheck
  '';

  meta = {
    description = "Oh My Pi coding agent CLI";
    homepage = "https://omp.sh";
    license = lib.licenses.mit;
    mainProgram = "omp";
    platforms = lib.attrNames nativesFor;
    sourceProvenance = [ lib.sourceTypes.binaryBytecode ];
  };
})
