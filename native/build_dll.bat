@echo off
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if errorlevel 1 exit /b 1

cl /LD /EHsc /O2 /I"third_party\spout\Spout-SDK-binaries\Libs_2-007-017\include\SpoutDX" native\spout_wrapper.cpp "third_party\spout\Spout-SDK-binaries\Libs_2-007-017\MD\lib\SpoutDX.lib" d3d11.lib d3dcompiler.lib dxgi.lib user32.lib psapi.lib /Fe:syngonia_spout.dll
if errorlevel 1 exit /b 1

copy /Y syngonia_spout.dll target\debug\syngonia_spout.dll
copy /Y "third_party\spout\Spout-SDK-binaries\Libs_2-007-017\MD\bin\SpoutDX.dll" target\debug\SpoutDX.dll
echo BUILD SUCCESSFUL
