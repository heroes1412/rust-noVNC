@echo off
rem ============================================================================
rem  clean.bat - remove build artifacts and toolchain cache
rem
rem    clean.bat          remove .\target and .\.tools  (~hundreds of MB)
rem    clean.bat target   remove .\target only          (keeps zig cache)
rem    clean.bat dist     remove .\dist only            (release binaries)
rem    clean.bat all      remove target, .tools, dist and novnc-client
rem    clean.bat --help   this text
rem
rem  Source code (src\, examples\, tests\, original-novnc\) is never touched.
rem  dist\ is kept unless you ask for it: it holds the release binaries.
rem
rem  Removing .tools only deletes the downloaded zig toolchain; build.bat
rem  reinstalls it automatically on the next cross build (needs Internet).
rem ============================================================================
setlocal EnableExtensions
cd /d "%~dp0"

if /i "%~1"=="--help" goto usage
if /i "%~1"=="-h" goto usage
if /i "%~1"=="all" goto all
if /i "%~1"=="target" goto target_only
if /i "%~1"=="dist" goto dist_only
if not "%~1"=="" (
    echo error: unknown option "%~1"
    echo run "clean.bat --help" for usage
    exit /b 2
)

call :remove "target"
if errorlevel 1 exit /b 1
call :remove ".tools"
if errorlevel 1 exit /b 1
echo.
echo clean: removed target ^(build cache^) and .tools ^(zig toolchain^)
echo        dist\ and all source folders were kept
echo        use "clean.bat target" to keep the zig cache, "clean.bat all" to wipe dist too
exit /b 0

:target_only
call :remove "target"
if errorlevel 1 exit /b 1
echo.
echo clean: removed target
echo        .tools\, dist\ and source were kept
exit /b 0

:dist_only
call :remove "dist"
if errorlevel 1 exit /b 1
echo.
echo clean: removed dist
echo        source, target\ and .tools\ were kept
exit /b 0

:all
call :remove "target"
if errorlevel 1 exit /b 1
call :remove ".tools"
if errorlevel 1 exit /b 1
call :remove "dist"
if errorlevel 1 exit /b 1
call :remove "novnc-client"
if errorlevel 1 exit /b 1
echo.
echo clean: removed target, .tools, dist and novnc-client
echo        the bundled original-novnc snapshot and all source were kept
exit /b 0

:remove
if exist "%~1" (
    echo removing %~1 ...
    rmdir /s /q "%~1"
    if errorlevel 1 (
        echo error: cannot remove "%~1" - close programs using it and retry.
        exit /b 1
    )
)
exit /b 0

:usage
echo.
echo clean.bat - remove build artifacts and toolchain cache
echo.
echo   clean.bat          remove .\target and .\.tools
echo   clean.bat target   remove .\target only, keep the zig cache
echo   clean.bat dist     remove .\dist only (release binaries)
echo   clean.bat all      remove target, .tools, dist and novnc-client
echo.
echo source code is never removed. dist\ is kept by the default command.
echo.
exit /b 0
