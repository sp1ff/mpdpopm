AC_INIT([mpdpopm], [0.1.1], [sp1ff@pobox.com])
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

AC_CONFIG_FILES([mpdpopm/src/vars.rs mpdpopm/Cargo.toml])
AC_CONFIG_FILES([Makefile mpdpopm/Makefile])

dnl Specify --enable-debug to make a development release.  By default,
dnl we build in public release mode.
AC_ARG_ENABLE(
    debug,
    AC_HELP_STRING([--enable-debug],
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
