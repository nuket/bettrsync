@echo off
rem bettrsync
rem Copyright (c) 2026 Max Vilimpoc
rem SPDX-License-Identifier: MIT
rem Builds bettrsync on Windows with the cargo on the PATH (rustup's
rem MSVC toolchain); the counterpart of build.sh.
rem usage: build.bat [--debug|--release]

setlocal

cd /d "%~dp0"

set "MODE=release"
set "CARGO_FLAGS=--release"

:parse_args
if "%~1"=="" goto done_args
if "%~1"=="--debug"   (set "MODE=debug"   & set "CARGO_FLAGS="          & shift & goto parse_args)
if "%~1"=="--release" (set "MODE=release" & set "CARGO_FLAGS=--release" & shift & goto parse_args)
echo Unknown argument: %~1 1>&2
echo Usage: build.bat [--debug^|--release] 1>&2
exit /b 1
:done_args

rem The revision stamped into the executable: BETTRSYNC_GIT_REVISION
rem when the caller knows it (systems-build.py, building a tree synced
rem without its git history), else this checkout's.
if defined BETTRSYNC_GIT_REVISION set GIT_REV=%BETTRSYNC_GIT_REVISION%
if not defined GIT_REV for /f "delims=" %%r in ('git rev-parse --short HEAD 2^>nul') do set GIT_REV=%%r
if not defined GIT_REV set GIT_REV=unknown
if not defined BETTRSYNC_GIT_REVISION for /f "delims=" %%r in ('git status --porcelain 2^>nul') do set GIT_REV=%GIT_REV%-modified

set BETTRSYNC_GIT_REVISION=%GIT_REV%
cargo build %CARGO_FLAGS%
if errorlevel 1 exit /b %errorlevel%

echo Binary: %CD%\target\%MODE%\bettrsync.exe
