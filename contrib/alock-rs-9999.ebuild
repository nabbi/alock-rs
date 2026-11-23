# Copyright 2026 Gentoo Authors
# Distributed under the terms of the GNU General Public License v2

EAPI=8

inherit cargo git-r3

DESCRIPTION="X11 screen locker with software ARGB cursor and animated console theme"
HOMEPAGE="https://github.com/nabbi/alock-rs"
EGIT_REPO_URI="https://github.com/nabbi/alock-rs"

LICENSE="MIT"
# Dependent crate licenses
LICENSE+=" Apache-2.0 BSD MIT Unicode-3.0 ZLIB"
SLOT="0"

DEPEND="sys-libs/pam"
RDEPEND="${DEPEND}"
BDEPEND=">=dev-lang/rust-1.88"

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

src_install() {
	cargo_src_install
	dodoc README.md docs/*.md
}
