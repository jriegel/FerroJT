@echo off
setlocal EnableExtensions

rem ===========================================================================
rem  FerroJT - build the Python wheels for PyPI (stable ABI, abi3-py310)
rem
rem  pyo3 is built with the "abi3-py310" feature, so ONE wheel per platform
rem  covers every CPython from 3.10 upwards - there is no wheel per Python
rem  version to build.
rem
rem  Output in dist/ :
rem    ferrojt-<ver>-cp310-abi3-win_amd64.whl                       Windows
rem    ferrojt-<ver>-cp310-abi3-manylinux_2_17_x86_64.whl           Linux (zig cross)
rem    ferrojt-<ver>.tar.gz                                        source distribution
rem
rem  Usage:
rem    build-wheel.bat            everything (windows + linux + sdist)
rem    build-wheel.bat windows    only the host (Windows) wheel
rem    build-wheel.bat linux      only the cross-compiled Linux wheel
rem    build-wheel.bat sdist      only the source distribution
rem
rem  Upload afterwards with:
rem    .venv\Scripts\python -m twine upload dist\*
rem ===========================================================================

cd /d "%~dp0" || exit /b 1

set "PY=%~dp0.venv\Scripts\python.exe"
set "VIRTUAL_ENV=%~dp0.venv"
set "OUT=%~dp0dist"
set "LINUX_TARGET=x86_64-unknown-linux-gnu"

set "WHAT=%~1"
if "%WHAT%"=="" set "WHAT=all"

rem --------------------------------------------------------------------------
rem 0. Prerequisites
rem --------------------------------------------------------------------------
if not exist "%PY%" goto :no_venv

echo == Build requirements =====================================================
"%PY%" -m pip install --upgrade pip || exit /b 1
"%PY%" -m pip install --upgrade "maturin>=1.15,<2.0" twine || exit /b 1

set "NEED_LINUX=0"
if not "%WHAT%"=="windows" if not "%WHAT%"=="sdist" set "NEED_LINUX=1"

if "%NEED_LINUX%"=="0" goto :prepare

rem zig is shipped as a Python wheel, so no system install is required.
where rustup >nul 2>&1 || goto :no_rustup
"%PY%" -m pip install --upgrade "maturin[zig]>=1.15,<2.0" || exit /b 1
rustup target add %LINUX_TARGET% || exit /b 1

rem maturin finds zig through PATH, not through the venv - the ziglang wheel
rem drops zig.exe into site-packages\ziglang.
if exist "%~dp0.venv\Lib\site-packages\ziglang\zig.exe" set "PATH=%~dp0.venv\Lib\site-packages\ziglang;%PATH%"

:prepare
rem --------------------------------------------------------------------------
rem 1. Fresh output directory
rem --------------------------------------------------------------------------
if exist "%OUT%" rmdir /s /q "%OUT%"
mkdir "%OUT%"

rem --------------------------------------------------------------------------
rem 2. Windows wheel - stable ABI, one wheel for all Python >= 3.10
rem --------------------------------------------------------------------------
if "%WHAT%"=="linux" goto :linux
if "%WHAT%"=="sdist" goto :sdist

echo.
echo == Windows wheel ==========================================================
"%PY%" -m maturin build --release --locked --strip -i "%PY%" -o "%OUT%" || exit /b 1

:linux
if "%WHAT%"=="sdist" goto :sdist

echo.
echo == Linux wheel, cross-compiled with zig ===================================
rem -i is required: without an interpreter maturin probes "python3", which on
rem Windows resolves to the Microsoft Store stub. The abi3 wheel is not bound to
rem that version - it is tagged cp310-abi3 and serves CPython 3.10 and up.
"%PY%" -m maturin build --release --locked --strip --target %LINUX_TARGET% --zig --compatibility manylinux_2_17 -i "%PY%" -o "%OUT%" || goto :linux_failed

rem Optional musllinux (Alpine) wheel - uncomment if you want it.
rem "%PY%" -m maturin build --release --locked --strip --target x86_64-unknown-linux-musl --zig --compatibility musllinux_1_1 -i "%PY%" -o "%OUT%" || exit /b 1

:sdist
echo.
echo == Source distribution ====================================================
"%PY%" -m maturin sdist -o "%OUT%" || exit /b 1

rem --------------------------------------------------------------------------
rem 3. Summary
rem --------------------------------------------------------------------------
echo.
echo == dist\ ==================================================================
dir /b "%OUT%"

"%PY%" -m pip show twine >nul 2>&1 || goto :done
echo.
echo == twine check ============================================================
"%PY%" -m twine check "%OUT%\*"

:done
echo.
echo Done. Upload with: "%PY%" -m twine upload "%OUT%\*"
echo        (maturin's own "upload" and "publish" commands are deprecated)
exit /b 0

rem --------------------------------------------------------------------------
rem Diagnostics
rem --------------------------------------------------------------------------
:no_venv
echo ERROR: no virtualenv found at .venv
echo        python -m venv .venv
echo        .venv\Scripts\python -m pip install -U pip
exit /b 1

:no_rustup
echo ERROR: rustup is not on PATH, the Linux cross build needs it.
echo        Run "build-wheel.bat windows" for the host wheel only.
exit /b 1

:linux_failed
echo.
echo The Linux cross build failed. Check that:
echo   - zig is installed:  "%PY%" -m pip install "maturin[zig]"
echo   - target installed:  rustup target add %LINUX_TARGET%
exit /b 1
