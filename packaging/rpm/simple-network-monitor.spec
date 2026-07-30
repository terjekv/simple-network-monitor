Name:           simple-network-monitor
Version:        %{?package_version}%{!?package_version:0.0.1}
Release:        %{?package_release}%{!?package_release:1%{?dist}}
Summary:        Simple ICMP and usage monitoring daemon with a JSON API

%global debug_package %{nil}
%global __requires_exclude ^rtld\\(GNU_HASH\\)$
%bcond_with local_cargo
%bcond_with prebuilt
%if %{with prebuilt}
# Preserve the already stripped and statically verified release executable.
%global __strip /bin/true
%endif

License:        MIT
Source0:        %{name}-%{version}.tar.gz
%if %{with prebuilt}
Source1:        %{name}
%endif

%if !%{with prebuilt}
%if !%{with local_cargo}
BuildRequires:  cargo
BuildRequires:  rust
%endif
BuildRequires:  gcc
%endif
BuildRequires:  systemd-rpm-macros
Requires(pre):  shadow-utils
%{?systemd_requires}

Requires:       iputils
Recommends:     openssh-clients

%description
simple-network-monitor monitors configured hosts with ICMP, optionally collects
usage data over SSH, stores history in SQLite, and exposes current state plus
history through a JSON API.

%prep
%autosetup

%build
%if %{with prebuilt}
:
%else
cargo build --release --locked
%endif

%install
%if %{with prebuilt}
install -Dpm0755 %{SOURCE1} \
    %{buildroot}%{_bindir}/simple-network-monitor
%else
install -Dpm0755 target/release/simple-network-monitor \
    %{buildroot}%{_bindir}/simple-network-monitor
%endif

install -Dpm0644 monitor.example.toml \
    %{buildroot}%{_sysconfdir}/simple-network-monitor/monitor.toml
sed -i 's#^database_path = .*$#database_path = "%{_sharedstatedir}/simple-network-monitor/network-monitor.sqlite3"#' \
    %{buildroot}%{_sysconfdir}/simple-network-monitor/monitor.toml

install -Dpm0644 packaging/systemd/simple-network-monitor.service \
    %{buildroot}%{_unitdir}/simple-network-monitor.service
sed -i 's#/usr/local/bin/simple-network-monitor#%{_bindir}/simple-network-monitor#' \
    %{buildroot}%{_unitdir}/simple-network-monitor.service

mkdir -p %{buildroot}%{_sharedstatedir}/simple-network-monitor

%check
%if %{with prebuilt}
%{buildroot}%{_bindir}/simple-network-monitor \
    --config monitor.example.toml \
    --verify-config-only
%else
cargo test --release --locked
%endif

%pre
getent group simple-network-monitor >/dev/null || \
    groupadd --system simple-network-monitor
getent passwd simple-network-monitor >/dev/null || \
    useradd --system \
        --gid simple-network-monitor \
        --home-dir %{_sharedstatedir}/simple-network-monitor \
        --shell /sbin/nologin \
        --comment "Simple Network Monitor" \
        simple-network-monitor

%post
%systemd_post simple-network-monitor.service

%preun
%systemd_preun simple-network-monitor.service

%postun
%systemd_postun_with_restart simple-network-monitor.service

%files
%license LICENSE
%doc README.md docs/architecture.md
%{_bindir}/simple-network-monitor
%{_unitdir}/simple-network-monitor.service
%config(noreplace) %{_sysconfdir}/simple-network-monitor/monitor.toml
%dir %attr(0750,simple-network-monitor,simple-network-monitor) %{_sharedstatedir}/simple-network-monitor

%changelog
* Wed Jun 10 2026 Simple Network Monitor Maintainers <root@localhost> - 0.0.1-1
- Initial RPM package
