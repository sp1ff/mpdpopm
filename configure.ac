AC_INIT([mpdpopm], [0.1.15], [sp1ff@pobox.com], [mpdpopm], [https://github.com/sp1ff/mpdpopm])
AC_CONFIG_AUX_DIR([build-aux])
AC_CONFIG_SRCDIR([mpdpopm/Cargo.toml.in])
AM_INIT_AUTOMAKE([-Wall -Werror -Wno-portability -Wno-override gnits std-options dist-bzip2 dist-xz])

AC_CHECK_PROG(CARGO, [cargo], [yes], [no])
AS_IF(test x$CARGO = xno,
    AC_MSG_ERROR([cargo is required])
)
AC_CHECK_PROG(RUSTC, [rustc], [yes], [no])
AS_IF(test x$RUSTC = xno,
    AC_MSG_ERROR([rustc is required])
)

dnl `vars.rs.am.in` contains Installation Directory Variables
dnl <https://www.gnu.org/software/autoconf/manual/autoconf-2.67/html_node/Installation-Directory-Variables.html#Installation-Directory-Variables>
dnl So, e.g. @sysconfdir@ will be replaced with ${prefix}/etc by Autoconf. This is so we can
dnl produce Makefiles that comply with the GCS in that the user can still change the installation
dnl location at make-time.
dnl
dnl I handle this by having Autconf process vars.rs.am.in to vars.rs.am at configure time, and
dnl adding a rule to Automake to build vars.rs from vars.rs.am at make-time.
dnl
dnl Ditto for etc/mppopmd.conf.am.in and etc/mppopmd.service.am.in
AC_CONFIG_FILES([mpdpopm/src/vars.rs.am])
AC_CONFIG_FILES([mpdpopm/mppopmd.conf.am:etc/mppopmd.conf.am.in])
AC_CONFIG_FILES([mpdpopm/mppopmd.service.am:etc/mppopmd.service.am.in])
AC_CONFIG_FILES([PKGBUILD:admin/PKGBUILD.in])
AC_CONFIG_FILES([Makefile mpdpopm/Makefile mpdpopm/Cargo.toml])

dnl Specify --enable-debug to make a development release.  By default,
dnl we build in public release mode.
AC_ARG_ENABLE(
    debug,
    AS_HELP_STRING([--enable-debug],
                   [Build Rust code with debugging information [default=no]]),
    [rust_debug=$enableval],
    [rust_debug=no])

AC_MSG_CHECKING(whether to build Rust code with debugging information)
if test "x$rust_debug" = "xyes" ; then
    AC_MSG_RESULT(yes)
    RUST_TARGET_SUBDIR=debug
else
    AC_MSG_RESULT(no)
    RUST_TARGET_SUBDIR=release
fi
AM_CONDITIONAL([RUST_DEBUG], [test "x$rust_debug" = "xyes"])

AC_SUBST([RUST_TARGET_SUBDIR])
AC_SUBST(PACKAGE_AUTHOR, 'Michael Herstine <sp1ff@pobox.com>')

AC_OUTPUT
