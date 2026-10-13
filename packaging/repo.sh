#!/usr/bin/env bash
# Build Linux packages (.deb, .rpm, Arch .pkg.tar.zst) for a release tag and
# publish them to the signed apt, dnf and pacman repositories served at
# https://repo.dixon.cx.
#
#   packaging/repo.sh          # the newest tag
#   packaging/repo.sh 0.1.0    # a given tag
#
# Builds from a clean export of the tag, so the working tree doesn't matter.
# The binary is built in an Ubuntu 22.04 container so it runs on glibc 2.35
# and later (Debian 12, Ubuntu 22.04, current Fedora and Arch). Every index is
# rebuilt over all the packages in the repository, so other projects
# publishing there with their copy of this script are kept.
#
# Needs: go (to run nfpm), docker, gpg, apt-ftparchive (apt-utils), flock.
#   REPO_DIR        where the repository lives (default below)
#   REPO_GNUPGHOME  GnuPG home holding the signing key (default below)
#
# The same script, with its own project block, is in dockertop, mapsize,
# vault and swaptop.
set -euo pipefail

# ---- project ---------------------------------------------------------------
PACKAGE=apex
CONFLICTS=
SUMMARY="A native, high-performance text editor"
HOMEPAGE=https://dixon.cx/apex
LICENSE=AGPL-3.0-or-later
# ponytail: amd64 only. A GUI binary links glibc and can't be cross-built
# statically like the CLI tools; add arm64 with a native arm64 builder.
ARCH=amd64
# ----------------------------------------------------------------------------

REPO_DIR=${REPO_DIR:-/home/andy/domains/repo.dixon.cx/public_html}
REPO_GNUPGHOME=${REPO_GNUPGHOME:-$HOME/.config/repo.dixon.cx/gnupg}
NFPM="go run github.com/goreleaser/nfpm/v2/cmd/nfpm@v2.41.1"
SUITE=stable
COMPONENT=main
ARCH_REPO=dixon # the pacman repository name: [dixon]

say() { echo "repo: $*" >&2; }
die() { say "$*"; exit 1; }

for tool in go docker gpg apt-ftparchive flock; do
	command -v $tool >/dev/null || die "needs $tool"
done
[[ -d $REPO_DIR ]] || die "$REPO_DIR does not exist"
export GNUPGHOME=$REPO_GNUPGHOME
KEY=$(gpg --list-secret-keys --with-colons 2>/dev/null | awk -F: '/^fpr/{print $10; exit}')
[[ -n $KEY ]] || die "no signing key in $GNUPGHOME"

top=$(git rev-parse --show-toplevel) || die "not in the apex repository"
cd "$top"
TAG=${1:-$(git describe --tags --abbrev=0)}
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null || die "no tag $TAG"
VERSION=${TAG#v}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/src"
git archive "$TAG" | tar -x -C "$WORK/src"
# The binary's --version comes from Cargo.toml; make it match the tag.
sed -i "0,/^version = .*/s//version = \"$VERSION\"/" "$WORK/src/Cargo.toml"
gpg --batch --armor --export-secret-keys "$KEY" >"$WORK/signing.asc"

# ---- build -----------------------------------------------------------------
say "building $PACKAGE $VERSION for $ARCH"
docker image inspect repo-tools:rust-jammy >/dev/null 2>&1 ||
	docker build -q -t repo-tools:rust-jammy - >/dev/null <<-'EOF'
	FROM ubuntu:22.04
	RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl build-essential pkg-config \
	    && rm -rf /var/lib/apt/lists/*
	ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
	RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable -c rustfmt,clippy \
	    && chmod -R a+rwX /usr/local/cargo /usr/local/rustup
	EOF
# The crate cache lives outside $WORK so later releases don't download again.
mkdir -p "$HOME/.cache/repo-tools/cargo"
docker run --rm --user "$(id -u):$(id -g)" -e CARGO_HOME=/cargo -e HOME=/tmp \
	-v "$HOME/.cache/repo-tools/cargo:/cargo" -v "$WORK/src:/src" -w /src repo-tools:rust-jammy \
	cargo build --release -p apex-app || die "build failed"

src=$WORK/src
{
	cat <<-EOF
	name: $PACKAGE
	arch: $ARCH
	platform: linux
	version: $VERSION
	release: 1
	section: editors
	maintainer: Andy Dixon <andy@dixon.cx>
	description: $SUMMARY
	homepage: $HOMEPAGE
	license: $LICENSE
	conflicts: [${CONFLICTS:-}]
	contents:
	  - {src: $src/target/release/apex, dst: /usr/bin/apex, file_info: {mode: 0755}}
	  - {src: $src/packaging/linux/apex.desktop, dst: /usr/share/applications/apex.desktop}
	  - {src: $src/packaging/linux/apex.metainfo.xml, dst: /usr/share/metainfo/cx.dixon.apex.metainfo.xml}
	  - {src: $src/assets/icons/apex.svg, dst: /usr/share/icons/hicolor/scalable/apps/apex.svg}
	  - {src: $src/README.md, dst: /usr/share/doc/apex/README.md}
	  - {src: $src/LICENSE, dst: /usr/share/doc/apex/LICENSE}
	EOF
	for s in 16 24 32 48 64 128 256 512; do
		echo "  - {src: $src/assets/icons/png/apex-$s.png, dst: /usr/share/icons/hicolor/${s}x$s/apps/apex.png}"
	done
	printf 'rpm:\n  signature:\n    key_file: %s\n' "$WORK/signing.asc"
} >"$WORK/nfpm.yaml"
mkdir -p "$WORK/out"
for fmt in deb rpm archlinux; do
	$NFPM pkg -f "$WORK/nfpm.yaml" -p $fmt -t "$WORK/out/" >/dev/null
done

# ---- publish ---------------------------------------------------------------
# One publisher at a time: the indexes cover every project's packages.
exec 9>"$REPO_DIR/../.publish.lock"
flock 9
say "publishing to $REPO_DIR"
gpg --batch --armor --export "$KEY" >"$REPO_DIR/dixon.asc"
gpg --batch --export "$KEY" >"$REPO_DIR/dixon.gpg"
sign() { gpg --batch --yes --local-user "$KEY" "$@"; }

# apt: pool/main/<letter>/<name>/, indexed by apt-ftparchive.
pool=$REPO_DIR/apt/pool/$COMPONENT/${PACKAGE:0:1}/$PACKAGE
mkdir -p "$pool"
cp "$WORK"/out/*.deb "$pool/"
(
	cd "$REPO_DIR/apt"
	# Built beside the live suite and swapped in, so clients never see half.
	dists=dists/.$SUITE.new
	rm -rf "$dists"
	debarchs=$(find pool -name '*.deb' | sed -E 's/.*_([^_]+)\.deb$/\1/' | sort -u | tr '\n' ' ')
	for a in $debarchs; do
		d=$dists/$COMPONENT/binary-$a
		mkdir -p "$d"
		apt-ftparchive --arch "$a" packages pool >"$d/Packages"
		gzip -9kn "$d/Packages"
	done
	apt-ftparchive \
		-o APT::FTPArchive::Release::Origin=repo.dixon.cx \
		-o APT::FTPArchive::Release::Label=repo.dixon.cx \
		-o APT::FTPArchive::Release::Suite=$SUITE \
		-o APT::FTPArchive::Release::Codename=$SUITE \
		-o APT::FTPArchive::Release::Components=$COMPONENT \
		-o "APT::FTPArchive::Release::Architectures=${debarchs% }" \
		release "$dists" >"$WORK/Release"
	mv "$WORK/Release" "$dists/Release"
	sign --clearsign -o "$dists/InRelease" "$dists/Release"
	sign --armor --detach-sign -o "$dists/Release.gpg" "$dists/Release"
	rm -rf "dists/.$SUITE.old"
	[[ -d dists/$SUITE ]] && mv "dists/$SUITE" "dists/.$SUITE.old"
	mv "$dists" "dists/$SUITE"
	rm -rf "dists/.$SUITE.old"
)

# dnf: every architecture in one directory; dnf picks its own.
mkdir -p "$REPO_DIR/rpm"
cp "$WORK"/out/*.rpm "$REPO_DIR/rpm/"
docker image inspect repo-tools:fedora >/dev/null 2>&1 ||
	docker build -q -t repo-tools:fedora - >/dev/null <<-'EOF'
	FROM fedora:42
	RUN dnf -y install createrepo_c && dnf clean all
	EOF
docker run --rm --user "$(id -u):$(id -g)" -v "$REPO_DIR/rpm:/repo" repo-tools:fedora \
	createrepo_c --quiet --update --general-compress-type=gz /repo
sign --armor --detach-sign -o "$REPO_DIR/rpm/repodata/repomd.xml.asc" "$REPO_DIR/rpm/repodata/repomd.xml"
cat >"$REPO_DIR/rpm/dixon.repo" <<EOF
[dixon]
name=repo.dixon.cx
baseurl=https://repo.dixon.cx/rpm
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=https://repo.dixon.cx/dixon.asc
EOF

# pacman: one directory per architecture, each package signed, then
# repo-add (from an Arch container) and a signed database.
added=
for pkg in "$WORK"/out/*.pkg.tar.zst; do
	a=$(basename "$pkg" .pkg.tar.zst)
	a=${a##*-}
	mkdir -p "$REPO_DIR/arch/$a"
	cp "$pkg" "$REPO_DIR/arch/$a/"
	sign --detach-sign -o "$REPO_DIR/arch/$a/$(basename "$pkg").sig" "$pkg"
	added+=" $a"
done
for a in $(tr ' ' '\n' <<<"$added" | sort -u); do
	dir=$REPO_DIR/arch/$a
	# A fresh database of every package here; -p keeps the newest version.
	rm -f "$dir/$ARCH_REPO".{db,files}{,.tar.gz}{,.sig}
	docker run --rm --user "$(id -u):$(id -g)" -e HOME=/tmp -v "$dir:/repo" -w /repo archlinux:latest \
		sh -c "repo-add --quiet --prevent-downgrade $ARCH_REPO.db.tar.gz *.pkg.tar.zst" 2>&1 |
		{ grep -v "newer version\|^$" >&2 || true; }
	for db in $ARCH_REPO.db $ARCH_REPO.files; do
		sign --detach-sign -o "$dir/$db.tar.gz.sig" "$dir/$db.tar.gz"
		ln -sfn "$db.tar.gz.sig" "$dir/$db.sig"
	done
	rm -f "$dir"/*.old "$dir"/*.old.sig
done

say "published $PACKAGE $VERSION: $(cd "$WORK/out" && ls | wc -l) packages"
