# Binary RPM built by scripts/package-linux.sh from a staged payload.
# gc_version, gc_release and gc_payload are passed with --define.
%global debug_package %{nil}
%global __os_install_post %{nil}

Name:           supersprinter
Version:        %{gc_version}
Release:        %{gc_release}
Summary:        RSVP speed reader
License:        GPL-3.0-only
URL:            https://github.com/piotreknow02/supersprinter

# Linked libraries (including the libav*/libsw* media-export closure) come
# from AutoReqProv. Runtime-loaded ones are declared here.
Requires:       hicolor-icon-theme
# The renderer tries Vulkan, then EGL/GL.
Requires:       (vulkan-loader or libglvnd-egl)
# Native Wayland/X11 keyboard handling.
Requires:       libxkbcommon

%description
Supersprinter is a rapid serial visual presentation (RSVP) speed reader.
It shows text one word at a time at a fixed pivot point, either in a GUI
window or exported to a video file.

%install
cp -a %{gc_payload}/. %{buildroot}/

%files
%{_bindir}/supersprinter
%{_datadir}/applications/supersprinter.desktop
%{_datadir}/icons/hicolor/*/apps/supersprinter.png
%license %{_datadir}/licenses/supersprinter/LICENSE
%doc %{_datadir}/doc/supersprinter/README.md
