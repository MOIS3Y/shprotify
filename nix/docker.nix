# Docker-compatible image built from the same package as `nix build`.
{
  pkgs,
  shprotify,
  cargoPackage,
}:
pkgs.dockerTools.buildLayeredImage {
  name = "ghcr.io/mois3y/shprotify";
  tag = cargoPackage.version;

  # Keep timestamps fixed so identical inputs produce identical images.
  created = "1970-01-01T00:00:01Z";
  contents = [
    shprotify
    pkgs.dockerTools.binSh
    pkgs.dockerTools.usrBinEnv
    pkgs.dockerTools.caCertificates
    (pkgs.dockerTools.fakeNss.override {
      extraPasswdLines = [
        "shprotify:x:1000:1000:Shprotify:/data:/sbin/nologin"
      ];
      extraGroupLines = [ "shprotify:x:1000:" ];
    })
  ];

  # Fakeroot records ownership in the archive without requiring root or KVM.
  fakeRootCommands = ''
    mkdir -p data config music tmp
    chmod 0755 config music
    chmod 0750 data
    chmod 1777 tmp
    chown 1000:1000 data
  '';

  config = {
    User = "1000:1000";
    WorkingDir = "/data";
    Entrypoint = [ "${shprotify}/bin/shprotify" ];
    Cmd = [ ];
    StopSignal = "SIGTERM";
    ExposedPorts."3000/tcp" = { };
    Volumes."/data" = { };
    Env = [
      "PATH=${shprotify}/bin:/bin"
      "HOME=/data"
      "XDG_CACHE_HOME=/data/cache"
      "TMPDIR=/tmp"
      "SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt"
      "SHPROTIFY_HTTP__BIND=0.0.0.0:3000"
      "SHPROTIFY_DATABASE__PATH=/data/shprotify.db"
      "SHPROTIFY_DOWNLOADS__DIRECTORY=/data/downloads"
      "SHPROTIFY_LOGGING__FORMAT=json"
    ];
    Labels = {
      "org.opencontainers.image.title" = cargoPackage.name;
      "org.opencontainers.image.description" = cargoPackage.description;
      "org.opencontainers.image.version" = cargoPackage.version;
      "org.opencontainers.image.source" = "https://github.com/MOIS3Y/shprotify";
      "org.opencontainers.image.licenses" = cargoPackage.license;
    };
  };
}
