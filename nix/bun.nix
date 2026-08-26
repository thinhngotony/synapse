# omp refuses to start on bun < 1.3.14 (it version-checks at runtime), and
# nixpkgs currently pins 1.3.13. nixpkgs' bun is a fetch-and-unzip of upstream's
# prebuilt binary, so overriding version + src is sufficient — nothing is compiled.
#
# Remove this file once nixpkgs ships bun >= 1.3.14 and use pkgs.bun directly.
{
  bun,
  fetchurl,
  stdenvNoCC,
}:
let
  version = "1.3.14";

  sources = {
    "x86_64-linux" = {
      asset = "bun-linux-x64.zip";
      hash = "sha256-lR7iruhV8IWVruxiJSJqKY0/6oOj3NZGXAnLzN9+hI8=";
    };
    "aarch64-linux" = {
      asset = "bun-linux-aarch64.zip";
      hash = "sha256-on/7Y6gxA3WDbg1vZorhf6jY0YuIw3yCHGUzGXOhmjs=";
    };
    "x86_64-darwin" = {
      asset = "bun-darwin-x64.zip";
      hash = "sha256-QYPfM3RiPlurMVxUfPoJdFM81FfYa3O2OfeoeXTNZjM=";
    };
    "aarch64-darwin" = {
      asset = "bun-darwin-aarch64.zip";
      hash = "sha256-2LliIYKK1vl6x6wKt+lYcjQa92MAHogD6CZ2UsJlJiA=";
    };
  };

  inherit (stdenvNoCC.hostPlatform) system;

  source = sources.${system} or (throw "bun ${version}: no upstream build for system ${system}");
in
bun.overrideAttrs (old: {
  inherit version;

  src = fetchurl {
    url = "https://github.com/oven-sh/bun/releases/download/bun-v${version}/${source.asset}";
    inherit (source) hash;
  };

  # Upstream's own version string is what omp gates on; make sure we get it.
  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    observed="$($out/bin/bun --version)"
    test "$observed" = "${version}" \
      || { echo "expected bun ${version}, got $observed"; exit 1; }
    runHook postInstallCheck
  '';
})
