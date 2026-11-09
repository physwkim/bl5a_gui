#!/bin/sh
# deploy/install.sh [--sim] [--python PYTHON]
#
# Builds the BL5A programs in place and installs them for this user: the
# user systemd services (~/.config/systemd/user/bl5a*), the settings
# (~/.config/bl5a/) and the GUI's desktop shortcut. --sim sets this
# computer up as the simulated beamline. Run it again after an update: it
# rebuilds and rewrites the services and the shortcut, and keeps the
# settings files that already exist.
set -eu

usage() {
    echo "usage: deploy/install.sh [--sim] [--python PYTHON]" >&2
    exit 2
}

BL5A=$(cd "$(dirname "$0")/.." && pwd)
sim=no
python=python3
while [ $# -gt 0 ]; do
    case $1 in
    --sim) sim=yes ;;
    --python)
        [ $# -ge 2 ] || usage
        python=$2
        shift
        ;;
    *) usage ;;
    esac
    shift
done

die() {
    echo "install.sh: $*" >&2
    exit 1
}
case "$BL5A" in
*[[:space:]]*) die "$BL5A holds a space; move the tree" ;;
esac
# mar345-ioc comes from the epics-rs-iocs revision Cargo.toml takes
# ad-mar345 from, so the IOC and bl5a-integrator decode frames alike.
rev=$(sed -n 's/^ad-mar345 = .*rev = "\([0-9a-f]*\)".*/\1/p' "$BL5A/Cargo.toml")
[ -n "$rev" ] || die "$BL5A/Cargo.toml: no ad-mar345 rev"
python=$(command -v "$python") || die "no Python '$python'; give one with --python"

echo "== Building bl5a-sim, bl5a-qs, bl5a-integrator and spec-ioc"
(cd "$BL5A" && cargo build --release --workspace)
echo "== Building mar345-ioc (epics-rs-iocs $rev)"
(cd "$BL5A" && cargo install --locked --git https://github.com/epics-rs/epics-rs-iocs \
    --rev "$rev" --root target/mar345-ioc --target-dir target/mar345-ioc/build mar345-ioc)

# An IOC reads its databases from the crate folders it was built from,
# which cargo's cache cleaning deletes after a month unused: copy each
# folder under cargo's home that it names to target/ioc-share, and
# target/ioc-share/<ioc>.env points the service there.
echo "== Copying the IOCs' database folders to target/ioc-share"
share=$BL5A/target/ioc-share
cargo_home=${CARGO_HOME:-$HOME/.cargo}
probe=$(mktemp)
echo epicsEnvShow >"$probe"
for ioc in spec-ioc:target/release/spec-ioc mar345-ioc:target/mar345-ioc/bin/mar345-ioc; do
    name=${ioc%%:*}
    rm -rf "${share:?}/$name" "$share/$name.env"
    mkdir -p "$share/$name"
    env -i NO_COLOR=1 "$BL5A/${ioc#*:}" "$probe" </dev/null |
        while IFS='=' read -r var dir; do
            case $dir in
            "$cargo_home"/*)
                cp -R "$dir" "$share/$name/$var"
                echo "$var=$share/$name/$var" >>"$share/$name.env"
                ;;
            esac
        done
    [ -s "$share/$name.env" ] || die "$name names no folder under $cargo_home"
done
rm -f "$probe"

if ! "$python" -I -c 'import silx, PySide6, repics, zmq, msgpack, numpy, fabio' 2>/dev/null; then
    echo "WARNING: $python lacks the GUI's packages (doc/install.md, section 3)" >&2
fi

conf=$HOME/.config/bl5a
mkdir -p "$conf"
if [ $sim = yes ]; then
    envs="bl5a spec-ioc mar345-ioc"
    data=$HOME/bl5a-data
    mkdir -p "$data"
else
    envs=bl5a
    data=/data/bl5a
fi
for e in $envs; do
    if [ -e "$conf/$e.env" ]; then
        echo "== Kept $conf/$e.env"
    else
        src=$BL5A/deploy/env/$([ $sim = yes ] && echo sim || echo beamline)/$e.env
        sed -e "s|@BL5A@|$BL5A|g" -e "s|@PYTHON@|$python|g" -e "s|@DATA@|$data|g" \
            "$src" >"$conf/$e.env"
        echo "== Wrote $conf/$e.env"
    fi
done

units=$HOME/.config/systemd/user
mkdir -p "$units"
for f in "$BL5A"/deploy/systemd/*; do
    sed "s|@BL5A@|$BL5A|g" "$f" >"$units/${f##*/}"
done
systemctl --user daemon-reload
if [ $sim = yes ]; then
    systemctl --user enable bl5a-sim.service bl5a-mar345-ioc.service
fi
echo "== Installed the services in $units"

apps=$HOME/.local/share/applications
mkdir -p "$apps"
sed "s|@BL5A@|$BL5A|g" "$BL5A/deploy/bl5a-gui.desktop" >"$apps/bl5a-gui.desktop"
desk=$(xdg-user-dir DESKTOP 2>/dev/null || echo "$HOME/Desktop")
if [ "$desk" != "$HOME" ] && [ -d "$desk" ]; then
    d=$desk/bl5a-gui.desktop
    cp "$apps/bl5a-gui.desktop" "$d"
    chmod +x "$d"
    # A desktop launches a desktop file without asking only once it is
    # trusted: GNOME by this flag, Xfce by the file's SHA-256.
    gio set "$d" metadata::trusted true 2>/dev/null || true
    gio set "$d" metadata::xfce-exe-checksum "$(sha256sum "$d" | cut -d' ' -f1)" 2>/dev/null || true
    echo "== Put the BL5A shortcut on $desk and in the applications menu"
else
    echo "== Put the BL5A shortcut in the applications menu (no desktop folder)"
fi

echo
if [ $sim = yes ]; then
    echo "Next: systemctl --user enable --now bl5a.target"
else
    echo "Next: edit $conf/bl5a.env, then doc/install.md section 6"
fi
