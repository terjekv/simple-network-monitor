Name:           simple-network-monitor
Version:        0.0.1
Release:        1%{?dist}
Summary:        Simple ICMP and usage monitoring daemon with a JSON API

%global debug_package %{nil}
%bcond_with local_cargo

License:        MIT
Source0:        %{name}-%{version}.tar.gz

%if !%{with local_cargo}
BuildRequires:  cargo
BuildRequires:  rust
%endif
BuildRequires:  systemd-rpm-macros
BuildRequires:  gcc
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
cargo build --release --locked

%install
install -Dpm0755 target/release/simple-network-monitor \
    %{buildroot}%{_bindir}/simple-network-monitor

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
cargo test --release --locked

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
