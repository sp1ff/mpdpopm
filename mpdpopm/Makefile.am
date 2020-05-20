if RUST_DEBUG
    CARGO_RELEASE_ARGS=
else
    CARGO_RELEASE_ARGS=--release
endif

CARGO_VERBOSE = $(cargo_verbose_$(V))
cargo_verbose_ = $(cargo_verbose_$(AM_DEFAULT_VERBOSITY))
cargo_verbose_0 =
cargo_verbose_1 = --verbose

EXTRA_DIST = Cargo.lock
bin_PROGRAMS = mppopmd mppopm

# `vars.rs.am.in` contains Installation Directory Variables
# <https://www.gnu.org/software/autoconf/manual/autoconf-2.67/html_node/Installation-Directory-Variables.html#Installation-Directory-Variables>
# So, e.g. @sysconfdir@ will be replaced with ${prefix}/etc by Autoconf. This is so we can
# produce Makefiles that comply with the GCS in that the user can still change the installation
# location at make-time.

# I handle this by having Autconf process vars.rs.am.in to vars.rs.am at configure time, and
# adding a rule to Automake to build vars.rs from vars.rs.am at make-time.
src/vars.rs: src/vars.rs.am
	sed -e "s|\$${prefix}|$(prefix)|g" $< > $@

CLEANFILES = src/vars.rs

common_sources = vars.rs commands.rs clients.rs ratings.rs playcounts.rs lib.rs

mppopmd_SOURCES = Cargo.toml $(common_sources:%=src/%) src/bin/mppopmd.rs

mppopm_SOURCES = Cargo.toml $(common_sources:%=src/%) src/bin/mppopm.rs

mppopmd$(EXEEXT): $(mppopmd_SOURCES)
	cd $(top_srcdir)/mpdpopm && \
	CARGO_TARGET_DIR=@abs_top_builddir@/mpdpopm/target cargo build $(CARGO_VERBOSE) $(CARGO_RELEASE_ARGS)

mppopm$(EXEEXT): $(mppopm_SOURCES)
	cd $(top_srcdir)/mpdpopm && \
	CARGO_TARGET_DIR=@abs_top_builddir@/mpdpopm/target cargo build $(CARGO_VERBOSE) $(CARGO_RELEASE_ARGS)

clean-local:
	cd $(top_srcdir)/mpdpopm && \
	CARGO_TARGET_DIR=@abs_top_builddir@/mpdpopm/target cargo clean

check-local:
	cd $(top_srcdir)/mpdpopm && \
	CARGO_TARGET_DIR=@abs_top_builddir@/mpdpopm/target cargo test

install-exec-local: mppopmd$(EXEEXT) mppopm$(EXEEXT)
	install -d $(DESTDIR)$(bindir)
	install -m 755 @abs_top_builddir@/mpdpopm/target/$(RUST_TARGET_SUBDIR)/mppopmd $(DESTDIR)$(bindir)/mppopmd
	install -m 755 @abs_top_builddir@/mpdpopm/target/$(RUST_TARGET_SUBDIR)/mppopm $(DESTDIR)$(bindir)/mppopm
