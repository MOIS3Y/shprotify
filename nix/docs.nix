# Static bilingual site, independent of the server's runtime closure.
{
  pkgs,
  basePath ? "/shprotify/",
}:
pkgs.buildNpmPackage {
  pname = "shprotify-docs";
  version = (builtins.fromJSON (builtins.readFile ../docs/package.json)).version;

  src = pkgs.lib.fileset.toSource {
    root = ../.;
    fileset = pkgs.lib.fileset.unions [
      ../docs
      ../examples/shprotify.toml
      ../migrations
      ../Cargo.toml
    ];
  };
  postUnpack = ''
    sourceRoot="$sourceRoot/docs"
  '';

  nodejs = pkgs.nodejs_24;
  npmDeps = pkgs.importNpmLock { npmRoot = ../docs; };
  npmConfigHook = pkgs.importNpmLock.npmConfigHook;
  npmInstallFlags = [ "--ignore-scripts" ];
  npmRebuildFlags = [ "--ignore-scripts" ];

  nativeBuildInputs = [
    pkgs.python3
    pkgs.graphviz
  ];
  DOCS_BASE = basePath;
  # Build logs are not an interactive terminal. Disable progress redraws.
  CI = "true";
  FONTCONFIG_FILE = pkgs.makeFontsConf {
    fontDirectories = [ pkgs.dejavu_fonts ];
  };

  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    cp -r .vitepress/dist/. "$out/"
    touch "$out/.nojekyll"
    runHook postInstall
  '';

  meta = {
    description = "English and Russian Shprotify documentation";
    license = pkgs.lib.licenses.gpl3Only;
  };
}
