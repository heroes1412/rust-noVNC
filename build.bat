@echo off
rem ============================================================================
rem  build.bat - build novnc-rs for Windows (x64/ARM64) and Linux (x64/ARM64)
rem
rem  Usage:
rem    build.bat                     build all four binaries + run tests
rem    build.bat --only win64        build selected targets (comma separated)
rem    build.bat --skip-tests        do not run the Rust test suite
rem    build.bat --update-client     git clone / git pull the noVNC client
rem    build.bat --help              this text
rem
rem  Targets: win64, winarm64, linux64, linuxarm64
rem
rem  Everything is self-contained: zig and cargo-zigbuild are downloaded into
rem  .\tools when a cross target needs them. No Python, WSL, Docker or Visual
rem  Studio ARM64 components are required.
rem
rem  Client source (in priority order):
rem    1. %%NOVNC_CLIENT_DIR%%
rem    2. .\novnc-client      - used when present, managed by --update-client
rem    3. .\original-novnc    - snapshot bundled with this repository
rem ============================================================================
setlocal EnableExtensions EnableDelayedExpansion
cd /d "%~dp0"

set "ROOT=%CD%"
set "DIST=%ROOT%\dist"
set "TOOLS=%ROOT%\.tools"
set "ZIG_VERSION=0.15.2"
set "ZIG_DIR=%TOOLS%\zig-x86_64-windows-%ZIG_VERSION%"
set "ZIG=%ZIG_DIR%\zig.exe"
set "ZIG_ZIP=%TOOLS%\zig-%ZIG_VERSION%.zip"
set "ZIG_URL=https://ziglang.org/download/%ZIG_VERSION%/zig-x86_64-windows-%ZIG_VERSION%.zip"
set "ZIG_SHA256=3a0ed1e8799a2f8ce2a6e6290a9ff22e6906f8227865911fb7ddedc3cc14cb0c"
set "ZIGBUILD_VERSION=0.24.1"
set "NOVNC_REPO=https://github.com/novnc/noVNC.git"

set "RUN_TESTS=1"
set "UPDATE_CLIENT=0"
set "ONLY="
set "DO_WIN64=0"
set "DO_WINARM64=0"
set "DO_LINUX64=0"
set "DO_LINUXARM64=0"

rem ---------------------------------------------------------------- arguments
:parse_args
if "%~1"=="" goto args_done
if /i "%~1"=="--help" goto usage
if /i "%~1"=="-h" goto usage
if /i "%~1"=="--skip-tests" (
    set "RUN_TESTS=0"
    shift
    goto parse_args
)
if /i "%~1"=="--update-client" (
    set "UPDATE_CLIENT=1"
    shift
    goto parse_args
)
if /i "%~1"=="--only" (
    if "%~2"=="" (
        echo error: --only needs a comma separated list, e.g. --only win64,linux64
        exit /b 2
    )
    set "ONLY=%~2"
    shift
    shift
    goto parse_args
)
echo error: unknown option "%~1"
echo run "build.bat --help" for usage
exit /b 2
:args_done

if not defined ONLY (
    set "DO_WIN64=1"
    set "DO_WINARM64=1"
    set "DO_LINUX64=1"
    set "DO_LINUXARM64=1"
    goto targets_done
)

set "ONLY=%ONLY:,= %"
for %%T in (%ONLY%) do (
    set "KNOWN=0"
    if /i "%%T"=="win64" (
        set "DO_WIN64=1"
        set "KNOWN=1"
    )
    if /i "%%T"=="winarm64" (
        set "DO_WINARM64=1"
        set "KNOWN=1"
    )
    if /i "%%T"=="linux64" (
        set "DO_LINUX64=1"
        set "KNOWN=1"
    )
    if /i "%%T"=="linuxarm64" (
        set "DO_LINUXARM64=1"
        set "KNOWN=1"
    )
    if /i "%%T"=="all" (
        set "DO_WIN64=1"
        set "DO_WINARM64=1"
        set "DO_LINUX64=1"
        set "DO_LINUXARM64=1"
        set "KNOWN=1"
    )
    if "!KNOWN!"=="0" (
        echo error: unknown target "%%T"
        echo valid targets: win64, winarm64, linux64, linuxarm64
        exit /b 2
    )
)
:targets_done

set "NEED_ZIG=0"
if not "%DO_WINARM64%%DO_LINUX64%%DO_LINUXARM64%"=="000" set "NEED_ZIG=1"

rem ------------------------------------------------------------- prerequisites
echo.
echo === novnc-rs build ============================================
echo.

where cargo >nul 2>&1
if errorlevel 1 (
    echo error: cargo was not found in PATH.
    echo        install Rust from https://rustup.rs and open a new terminal.
    exit /b 1
)
for /f "delims=" %%V in ('cargo --version') do set "CARGO_VERSION=%%V"
for /f "delims=" %%V in ('rustc --version') do set "RUSTC_VERSION=%%V"
echo %CARGO_VERSION%
echo %RUSTC_VERSION%

for /f "tokens=2 delims== " %%V in ('findstr /b /c:"version" Cargo.toml') do set "APP_VERSION=%%V"
set "APP_VERSION=%APP_VERSION:"=%"
if not defined APP_VERSION set "APP_VERSION=0.0.0"

rem ------------------------------------------------------------ client source
if "!UPDATE_CLIENT!"=="1" call :update_client
if errorlevel 1 exit /b 1

call :resolve_client
if errorlevel 1 exit /b 1
set "NOVNC_CLIENT_DIR=!CLIENT_DIR!"
echo client: !CLIENT_DIR!  ^(noVNC !CLIENT_VERSION!^)
echo.

rem --------------------------------------------------------------- rust targets
call :rustup_target x86_64-pc-windows-msvc
if errorlevel 1 exit /b 1
if "%DO_WINARM64%"=="1" (
    call :rustup_target aarch64-pc-windows-gnullvm
    if errorlevel 1 exit /b 1
)
if "%DO_LINUX64%"=="1" (
    call :rustup_target x86_64-unknown-linux-musl
    if errorlevel 1 exit /b 1
)
if "%DO_LINUXARM64%"=="1" (
    call :rustup_target aarch64-unknown-linux-musl
    if errorlevel 1 exit /b 1
)

rem --------------------------------------------------------------- cross tools
if "%NEED_ZIG%"=="1" (
    call :ensure_zig
    if errorlevel 1 exit /b 1
    call :ensure_zigbuild
    if errorlevel 1 exit /b 1
    set "PATH=%ZIG_DIR%;%PATH%"
)

rem -------------------------------------------------------------------- tests
if "%RUN_TESTS%"=="1" (
    echo.
    echo === tests ======================================================
    set "RUSTFLAGS=-D warnings"
    cargo test
    if errorlevel 1 goto fail_tests
    set "RUSTFLAGS="
)

rem ------------------------------------------------------------------- builds
if not exist "%DIST%" mkdir "%DIST%"
set "FAILED="

if "%DO_WIN64%"=="1" (
    echo.
    echo === windows amd64 ^(x86_64-pc-windows-msvc^) =====================
    set "RUSTFLAGS=-D warnings"
    cargo build --release --target x86_64-pc-windows-msvc
    if errorlevel 1 goto fail_win64
    call :copy_artifact "target\x86_64-pc-windows-msvc\release\novnc.exe" "novnc-%APP_VERSION%-windows-amd64.exe"
    if errorlevel 1 goto fail_win64
)

if "%DO_WINARM64%"=="1" (
    echo.
    echo === windows arm64 ^(aarch64-pc-windows-gnullvm^) =================
    rem The gnullvm toolchain expects LLVM's libunwind; zig ships one.
    set "RUSTFLAGS=-D warnings -C link-arg=-lunwind"
    cargo zigbuild --release --target aarch64-pc-windows-gnullvm
    if errorlevel 1 goto fail_winarm64
    set "RUSTFLAGS="
    call :copy_artifact "target\aarch64-pc-windows-gnullvm\release\novnc.exe" "novnc-%APP_VERSION%-windows-arm64.exe"
    if errorlevel 1 goto fail_winarm64
)

if "%DO_LINUX64%"=="1" (
    echo.
    echo === linux amd64 ^(x86_64-unknown-linux-musl^) ====================
    set "RUSTFLAGS=-D warnings"
    cargo zigbuild --release --target x86_64-unknown-linux-musl
    if errorlevel 1 goto fail_linux64
    call :copy_artifact "target\x86_64-unknown-linux-musl\release\novnc" "novnc-%APP_VERSION%-linux-amd64"
    if errorlevel 1 goto fail_linux64
)

if "%DO_LINUXARM64%"=="1" (
    echo.
    echo === linux arm64 ^(aarch64-unknown-linux-musl^) ====================
    set "RUSTFLAGS=-D warnings"
    cargo zigbuild --release --target aarch64-unknown-linux-musl
    if errorlevel 1 goto fail_linuxarm64
    call :copy_artifact "target\aarch64-unknown-linux-musl\release\novnc" "novnc-%APP_VERSION%-linux-arm64"
    if errorlevel 1 goto fail_linuxarm64
)

rem ------------------------------------------------------------------ checksums
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ErrorActionPreference='Stop';" ^
  "Get-ChildItem -Path '%DIST%' -File | Where-Object { $_.Name -ne 'SHA256SUMS.txt' } | Sort-Object Name | ForEach-Object { '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name } | Set-Content -Encoding ASCII '%DIST%\SHA256SUMS.txt'"
if errorlevel 1 (
    echo warning: could not write dist\SHA256SUMS.txt
)

rem ------------------------------------------------------------------- summary
echo.
echo === done =======================================================
echo output: %DIST%
echo.
for %%F in ("%DIST%\novnc-*") do echo   %%~nxF  %%~zF bytes
echo.
echo run:
echo   novnc-%APP_VERSION%-windows-amd64.exe --vnc localhost:5900
echo   then open http://127.0.0.1:6080/vnc.html
echo ================================================================
exit /b 0

rem ---------------------------------------------------------------- subroutines

:update_client
where git >nul 2>&1
if errorlevel 1 (
    echo error: --update-client needs git in PATH.
    exit /b 1
)
if exist "novnc-client\.git" (
    echo === updating novnc-client =====================================
    git -C novnc-client fetch --prune --tags
    if errorlevel 1 exit /b 1
    git -C novnc-client pull --ff-only
    if errorlevel 1 exit /b 1
) else (
    if exist "novnc-client" (
        echo error: .\novnc-client exists but is not a git checkout.
        echo        move it away or delete it, then run --update-client again.
        exit /b 1
    )
    echo === cloning noVNC =============================================
    git clone --depth 1 "%NOVNC_REPO%" novnc-client
    if errorlevel 1 exit /b 1
)
exit /b 0

:resolve_client
set "CLIENT_DIR="
if defined NOVNC_CLIENT_DIR (
    set "CLIENT_DIR=%NOVNC_CLIENT_DIR%"
) else if exist "novnc-client\vnc.html" (
    set "CLIENT_DIR=%ROOT%\novnc-client"
) else if exist "original-novnc\vnc.html" (
    set "CLIENT_DIR=%ROOT%\original-novnc"
)
if not defined CLIENT_DIR (
    echo error: no noVNC client found.
    echo        run "build.bat --update-client" or set NOVNC_CLIENT_DIR.
    exit /b 1
)
if not exist "!CLIENT_DIR!\vnc.html" (
    echo error: "!CLIENT_DIR!" does not contain vnc.html.
    exit /b 1
)
set "CLIENT_VERSION=unknown"
for /f "usebackq delims=" %%V in (`powershell -NoProfile -Command "(Get-Content -Raw -LiteralPath '!CLIENT_DIR!\package.json' | ConvertFrom-Json).version"`) do set "CLIENT_VERSION=%%V"
exit /b 0

:rustup_target
for /f "delims=" %%T in ('rustup target list --installed') do (
    if /i "%%T"=="%~1" exit /b 0
)
echo adding rust target %~1 ...
rustup target add %~1
if errorlevel 1 (
    echo error: cannot install rust target %~1
    exit /b 1
)
exit /b 0

:ensure_zig
if exist "%ZIG%" (
    call :zig_version
    if "!ZIG_FOUND!"=="%ZIG_VERSION%" (
        echo zig: !ZIG_FOUND!
        exit /b 0
    )
)
echo === installing zig %ZIG_VERSION% ================================
if not exist "%TOOLS%" mkdir "%TOOLS%"
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ErrorActionPreference='Stop';" ^
  "if (-not (Test-Path '%ZIG_ZIP%')) { Write-Host 'downloading %ZIG_URL%'; Invoke-WebRequest -Uri '%ZIG_URL%' -OutFile '%ZIG_ZIP%' -UseBasicParsing };" ^
  "$h=(Get-FileHash '%ZIG_ZIP%' -Algorithm SHA256).Hash.ToLower();" ^
  "if ($h -ne '%ZIG_SHA256%') { Remove-Item '%ZIG_ZIP%' -Force; throw 'zig checksum mismatch, download removed' };" ^
  "if (-not (Test-Path '%ZIG%')) { tar -xf '%ZIG_ZIP%' -C '%TOOLS%' };" ^
  "if (-not (Test-Path '%ZIG%')) { throw 'zig extraction failed' }"
if errorlevel 1 (
    echo error: zig installation failed.
    echo        download %ZIG_URL% manually and extract it into .tools
    exit /b 1
)
call :zig_version
if not "!ZIG_FOUND!"=="%ZIG_VERSION%" (
    echo error: expected zig %ZIG_VERSION%, found "!ZIG_FOUND!"
    exit /b 1
)
echo zig: !ZIG_FOUND!
exit /b 0

:zig_version
set "ZIG_FOUND="
if not exist "%ZIG%" exit /b 0
"%ZIG%" version > "%TEMP%\novnc-zig-version.txt" 2>nul
if errorlevel 1 exit /b 0
set /p ZIG_FOUND=<"%TEMP%\novnc-zig-version.txt"
del "%TEMP%\novnc-zig-version.txt" >nul 2>&1
exit /b 0

:ensure_zigbuild
for /f "delims=" %%V in ('cargo zigbuild --help 2^>nul') do set "ZIGBUILD_OK=1"
if defined ZIGBUILD_OK (
    echo cargo-zigbuild: installed
    set "ZIGBUILD_OK="
    exit /b 0
)
echo === installing cargo-zigbuild %ZIGBUILD_VERSION% ==================
cargo install cargo-zigbuild --version %ZIGBUILD_VERSION% --locked
if errorlevel 1 (
    echo error: cargo install cargo-zigbuild failed.
    exit /b 1
)
exit /b 0

:copy_artifact
if not exist "%~1" (
    echo error: expected build output "%~1" was not produced.
    exit /b 1
)
copy /y "%~1" "%DIST%\%~2" >nul
if errorlevel 1 (
    echo error: cannot copy "%~1" to dist.
    exit /b 1
)
echo artifact: %~2
exit /b 0

:fail_tests
echo.
echo build failed: tests did not pass.
exit /b 1

:fail_win64
echo.
echo build failed: windows amd64.
exit /b 1

:fail_winarm64
echo.
echo build failed: windows arm64.
exit /b 1

:fail_linux64
echo.
echo build failed: linux amd64.
exit /b 1

:fail_linuxarm64
echo.
echo build failed: linux arm64.
exit /b 1

:usage
echo.
echo build.bat - build novnc-rs
echo.
echo   build.bat                       build all targets and run tests
echo   build.bat --only win64          build only Windows amd64
echo   build.bat --only winarm64,linux64
echo   build.bat --skip-tests          skip cargo test
echo   build.bat --update-client       git clone / git pull .\novnc-client
echo.
echo targets: win64 winarm64 linux64 linuxarm64
echo.
echo client resolution: NOVNC_CLIENT_DIR, then .\novnc-client, then .\original-novnc
echo cross tooling: zig %ZIG_VERSION% + cargo-zigbuild %ZIGBUILD_VERSION% are
echo installed automatically into .\tools and %%USERPROFILE%%\.cargo
echo.
exit /b 0
