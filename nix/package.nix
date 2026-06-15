{
  lib,
  craneLib,
  rustPlatform,
  features ? [ ],
  package ? null,
  pkg-config,
  openssl,
  cmake,
  version ? "dev",
  ...
}:
let
  pname = if package == null then "rlamus" else package;
  otfCargoOrJsSource =
    path: type: (builtins.match ".*/.*\\.(otf|js)$" path != null) || (craneLib.filterCargoSources path type);

  nativeBuildInputs = [
    pkg-config
  ];

  buildInputs = [
    openssl
  ];

  # Common args shared between dep-only and full builds
  commonArgs = {
    inherit
      nativeBuildInputs
      buildInputs
      pname
      ;

    cargoExtraArgs =
      lib.concatMapStringsSep " " (f: "--features ${f}") features
      + lib.concatStringsSep " " (lib.optional (package != null) "-p ${package}");
    # Tell crane not to run tests in the build phase
    doCheck = false;
  };

  # Build only dependencies first (allows caching the heavy compile step)
  cargoArtifacts = craneLib.buildDepsOnly (
    commonArgs
    // {
      src = craneLib.cleanCargoSource ../.;
      version = "0.0.0";
    }
  );
in
craneLib.buildPackage (
  commonArgs
  // {
    inherit
      cargoArtifacts
      pname
      version
      ;
    src = lib.sources.cleanSourceWith {
      src = ../.;
      filter = otfCargoOrJsSource;
      name = "source";
    };
    APP_VERSION = version;
    meta = {
      mainProgram = pname;
    };
  }
)
