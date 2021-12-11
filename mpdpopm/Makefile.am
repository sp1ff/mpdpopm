if RUST_DEBUG
    CARGO_RELEASE_ARGS=
else
    CARGO_RELEASE_ARGS=--release
endif

CARGO_VERBOSE = $(cargo_verbose_$(V))
cargo_verbose_ = $(cargo_verbose_$(AM_DEFAULT_VERBOSITY))
cargo_verbose_0 =
cargo_verbose_1 = --verbose

EXTRA_DIST = Cargo.lock $(builddir)/mppopmd.conf $(builddir)/mppopmd.service
bin_PROGRAMS = mppopmd mppopm

# `vars.rs.am.in` contains Installation Directory Variables
# <https://www.gnu.org/software/autoconf/manual/autoconf-2.67/html_node/Installation-Directory-Variables.html#Installation-Directory-Variables>
# So, e.g. @sysconfdir@ will be replaced with ${prefix}/etc by Autoconf. This is so we can
# produce Makefiles that comply with the GCS in that the user can still change the installation
# location at make-time.

# I handle this by having Autconf process vars.rs.am.in to vars.rs.am at configure time, and
# adding a rule to Automake to build the final rust file from vars.rs.am at make-time.
# The name is ridiculous, but if I call it vars.rs, make gets confused(?).
$(builddir)/src/flapdoodle.rs: $(builddir)/src/vars.rs.am
	sed -e "s|\$${prefix}|$(prefix)|g" $< > $@

$(builddir)/mppopmd.conf: $(builddir)/mppopmd.conf.am
	sed -e "s|\$${prefix}|$(prefix)|g" $< > $@

$(builddir)/mppopmd.service: $(builddir)/mppopmd.service.am
	sed -e "s|\$${prefix}|$(prefix)|g" $< > $@.$$$$ && \
	sed -e "s|\$${exec_prefix}|$(prefix)|g" $@.$$$$ > $@  && \
	rm $@.$$$$

CLEANFILES = $(builddir)/src/flapdoodle.rs $(builddir)/mppopmd.conf $(builddir)/mppopmd.service

DISTCLEANFILES = $(builddir)/Cargo.toml 

common_sources = clients.rs	\
		 commands.rs	\
		 config.rs      \
		 lib.rs		\
		 messages.rs    \
		 playcounts.rs	\
		 ratings.rs     \
		 filters_ast.rs

mppopmd_SOURCES = build.rs $(srcdir)/Cargo.toml $(common_sources:%=src/%) src/bin/mppopmd.rs src/filters.lalrpop $(builddir)/mppopmd.conf $(builddir)/mppopmd.service

mppopm_SOURCES = build.rs $(srcdir)/Cargo.toml $(common_sources:%=src/%) src/bin/mppopm.rs

# If the source tree is writable, copy Cargo.toml back into it, so that cargo will work
$(srcdir)/Cargo.toml: $(builddir)/Cargo.toml
	if test -w $(srcdir); then cp -v $(builddir)/Cargo.toml $(srcdir); fi

# If the source tree is writable, vars.rs back into it, so that cargo will work
$(srcdir)/src/vars.rs: $(builddir)/src/flapdoodle.rs
	if test -w $(srcdir); then cp -v $(builddir)/src/flapdoodle.rs $(srcdir)/src/vars.rs; fi

# Regardless, make sure they get into the distribution-- vars.rs will be built at install
dist-hook:
	cp -v Cargo.toml $(distdir)/mpdpopm
	cp -v src/flapdoodle.rs $(distdir)/src/vars.rs
	cp -v mppopmd.{conf,service} $(disdir)

mppopmd$(EXEEXT): $(mppopmd_SOURCES) $(srcdir)/Cargo.toml $(srcdir)/src/vars.rs
	cd $(top_srcdir)/mpdpopm && \
	CARGO_TARGET_DIR=@abs_top_builddir@/mpdpopm/target cargo build $(CARGO_VERBOSE) $(CARGO_RELEASE_ARGS)

mppopm$(EXEEXT): $(mppopm_SOURCES) $(srcdir)/Cargo.toml $(srcdir)/src/vars.rs
	cd $(top_srcdir)/mpdpopm && \
	CARGO_TARGET_DIR=@abs_top_builddir@/mpdpopm/target cargo build $(CARGO_VERBOSE) $(CARGO_RELEASE_ARGS)

clean-local:
	cd $(top_srcdir)/mpdpopm && \
	CARGO_TARGET_DIR=@abs_top_builddir@/mpdpopm/target cargo clean

check-local:
	cd $(top_srcdir)/mpdpopm && \
	CARGO_TARGET_DIR=@abs_top_builddir@/mpdpopm/target cargo test $(CARGO_VERBOSE) $(CARGO_RELEASE_ARGS) $(CARGO_FEATURE_ARGS)

install-exec-local: mppopmd$(EXEEXT) mppopm$(EXEEXT)
	install -d $(DESTDIR)$(bindir)
	install -m 755 @abs_top_builddir@/mpdpopm/target/$(RUST_TARGET_SUBDIR)/mppopmd $(DESTDIR)$(bindir)/mppopmd
	install -m 755 @abs_top_builddir@/mpdpopm/target/$(RUST_TARGET_SUBDIR)/mppopm $(DESTDIR)$(bindir)/mppopm
	install -d $(DESTDIR)$(docdir)/examples
	install -m 644 @abs_top_builddir@/mpdpopm/mppopmd.conf $(DESTDIR)$(docdir)/examples/mppopmd.conf
	install -d $(DESTDIR)$(libdir)/systemd/system
	install -m 644 @abs_top_builddir@/mpdpopm/mppopmd.service $(DESTDIR)$(libdir)/systemd/system/mppopmd.service
	install -d $(DESTDIR)$(libdir)/systemd/user
	install -m 644 @abs_top_builddir@/mpdpopm/mppopmd.service $(DESTDIR)$(libdir)/systemd/user/mppopmd.service

uninstall-local:
	rm -f $(DESTDIR)$(libdir)/systemd/user/mppopmd.service
	rm -f $(DESTDIR)$(libdir)/systemd/system/mppopmd.service
	rm -f $(DESTDIR)$(docdir)/examples/mppopmd.conf
	rmdir $(DESTDIR)$(docdir)/examples
