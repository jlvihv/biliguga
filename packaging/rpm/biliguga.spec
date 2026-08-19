Name:           biliguga
Version:        0.1.1
Release:        1%{?dist}
Summary:        Bilibili desktop client 哔哩咕嘎
License:        MIT
URL:            https://github.com/jlvihv/biliguga
Source0:        biliguga
Source1:        biliguga.desktop

Requires:       mpv-libs
Requires:       fontconfig
Requires:       freetype
Requires:       libX11
Requires:       libxkbcommon
Requires:       libxkbcommon-x11
Requires:       wayland
Requires:       libxcb
Requires:       vulkan-loader

%description
A small native Bilibili client built with Rust, GPUI and libmpv.

%install
install -D -m 0755 %{_sourcedir}/biliguga %{buildroot}%{_bindir}/biliguga
install -D -m 0644 %{_sourcedir}/biliguga.desktop %{buildroot}%{_datadir}/applications/biliguga.desktop

%files
%{_bindir}/biliguga
%{_datadir}/applications/biliguga.desktop

%changelog
* Wed Aug 19 2026 jlvihv - 0.1.1-1
- Initial package.
