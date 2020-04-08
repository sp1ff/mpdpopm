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
bin_PROGRAMS = mpdpopm

mpdpopm_SOURCES = Cargo.toml src/vars.rs src/lib.rs src/client.rs src/bin/mpdpopm.rs

mpdpopm$(EXEEXT): $(mpdpopm_SOURCES)
	cd $(top_srcdir)/mpdpopm && \
	CARGO_TARGET_DIR=@abs_top_builddir@/mpdpopm/target cargo build $(CARGO_VERBOSE) $(CARGO_RELEASE_ARGS)

clean-local:
	cd $(top_srcdir)/mpdpopm && \
	CARGO_TARGET_DIR=@abs_top_builddir@/mpdpopm/target cargo clean

check-local:
	cd $(top_srcdir)/mpdpopm && \
	CARGO_TARGET_DIR=@abs_top_builddir@/mpdpopm/target cargo test

install-exec-local: mpdpopm$(EXEEXT)
	install -d $(DESTDIR)$(bindir)
	install -m 755 @abs_top_builddir@/mpdpopm/target/$(RUST_TARGET_SUBDIR)/mpdpopm $(DESTDIR)$(bindir)/mpdpopm
