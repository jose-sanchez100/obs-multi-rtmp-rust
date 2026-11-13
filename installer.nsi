; NSIS installer for obs-multi-rtmp (Rust), modeled on the original plugin's installer.nsi.
; Installs to %ALLUSERSPROFILE%\obs-studio\plugins\obs-multi-rtmp, the per-machine
; plugin folder OBS Studio 28+ scans. Built by plugin/package.ps1.
;
; Differences from the original:
; - Removing deletes only the obs-multi-rtmp folder, never the whole plugins folder.
; - Refuses to install or remove while OBS has the plugin loaded.

!ifndef PLUGIN_SOURCE_DIR
    !define PLUGIN_SOURCE_DIR "target\package\obs-multi-rtmp"
!endif
!ifndef OUT_FILE
    !define OUT_FILE "obs-multi-rtmp-setup.exe"
!endif
!ifndef VERSION
    !define VERSION "0.0.0"
!endif

OutFile "${OUT_FILE}"

Unicode true
RequestExecutionLevel user

SetDatablockOptimize on
SetCompress auto
SetCompressor /SOLID lzma

Name "obs-multi-rtmp ${VERSION}"
Caption "Multiple RTMP Output Plugin for OBS Studio (Rust) ${VERSION}"
Icon "${NSISDIR}\Contrib\Graphics\Icons\win-install.ico"

Var /Global DefInstDir
Var /Global PluginDir

; Aborts with a message if OBS has the plugin DLL open (renaming a loaded DLL fails).
Function EnsureObsClosed
    IfFileExists "$PluginDir\bin\64bit\obs-multi-rtmp.dll" 0 done
    ClearErrors
    Rename "$PluginDir\bin\64bit\obs-multi-rtmp.dll" "$PluginDir\bin\64bit\obs-multi-rtmp.dll.check"
    IfErrors 0 renamed
        MessageBox MB_OK|MB_ICONSTOP "OBS Studio is running. Close it and run the installer again."
        Quit
    renamed:
    Rename "$PluginDir\bin\64bit\obs-multi-rtmp.dll.check" "$PluginDir\bin\64bit\obs-multi-rtmp.dll"
    done:
FunctionEnd

Function .onInit
    ReadEnvStr $0 "ALLUSERSPROFILE"
    StrCpy $DefInstDir "$0\obs-studio\plugins"
    StrCpy $PluginDir "$DefInstDir\obs-multi-rtmp"
    StrCpy $INSTDIR "$DefInstDir"

    IfFileExists "$PluginDir\*.*" AskUninst DontAskUninst
    AskUninst:
        MessageBox MB_YESNOCANCEL|MB_ICONQUESTION "obs-multi-rtmp is already installed.$\r$\n$\r$\nYes = Install / update$\r$\nNo = Remove$\r$\nCancel = Exit" IDYES NotDoUninst IDNO DoUninst
        Quit
    DoUninst:
        Call EnsureObsClosed
        RMDir /r "$PluginDir"
        IfFileExists "$PluginDir\*.*" 0 +3
            MessageBox MB_OK|MB_ICONSTOP "Could not remove $PluginDir completely."
            Quit
        MessageBox MB_OK|MB_ICONINFORMATION "obs-multi-rtmp has been removed.$\r$\nYour targets are kept in the OBS profile (obs-multi-rtmp.json)."
        Quit
    NotDoUninst:
        Call EnsureObsClosed
    DontAskUninst:
FunctionEnd

Function onDirPageLeave
    StrCmp "$INSTDIR" "$DefInstDir" DirNotModified DirModified
    DirModified:
        MessageBox MB_OK|MB_ICONSTOP "Please don't change the install directory."
        Abort
    DirNotModified:
FunctionEnd

Page directory "" "" onDirPageLeave
Page instfiles

Section
    ; Replace the old files completely (stale locale files or DLLs from older versions).
    RMDir /r "$PluginDir"
    SetOutPath "$INSTDIR"
    File /r "${PLUGIN_SOURCE_DIR}"
SectionEnd
