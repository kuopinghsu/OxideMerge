# Sourced as root by linux.sh: starts the servers that the tests ignored in a normal run
# connect to, all on 127.0.0.1 and all signing in as `tester` with the password this sets in
# $test_password:
#
#   2222  OpenSSH: public keys only, a host certificate, TCP forwarding (for jump hosts)
#   2223  OpenSSH: keyboard-interactive only, through PAM
#   2121  vsftpd: FTP and explicit FTPS, serving /tmp/ftptest/vsftpd
#   2124  vsftpd: explicit FTPS asking for a client certificate, the same folder
#   2122  ProFTPD: FTP and explicit FTPS, serving /tmp/ftptest/proftpd
#   2123  ProFTPD: implicit FTPS, the same folder
#   1080  SOCKS5 (Dante), and 1081 the same with sign-in
#   8889  HTTP CONNECT (tinyproxy), and 8888 the same with sign-in
#
# The FTP servers' certificate is pinned in /tmp/ftptest/known_certs, and pin_ssh_servers
# pins the SSH host key.

start_servers() {
  test_password=$(od -An -N12 -tx1 /dev/urandom | tr -d ' \n')
  echo "tester:$test_password" | chpasswd
  start_ssh_servers
  start_ftp_servers
  start_proxies
  sleep 1
}

start_ssh_servers() {
  local t=/tmp/sshtest key
  rm -rf "$t"
  mkdir -p "$t/data/sub"
  (
    cd "$t"
    for key in hostkey userkey ca; do ssh-keygen -q -t ed25519 -N '' -f "$key"; done
    ssh-keygen -q -t ed25519 -N pw -f enckey
    ssh-keygen -q -t ed25519 -N other -f strangerkey
    ssh-keygen -q -s ca -h -I test-host -n certified.test -V -5m:+1h hostkey.pub
    cat userkey.pub enckey.pub >authorized_keys
    printf 'hello remote\n' >data_file.txt
    printf 'a\n' >data/a.txt
    printf 'nested\n' >data/sub/b.txt
    ln -s sub data/link_to_sub
    ln -s ../data_file.txt data/link_to_file
    ln -s nowhere data/dangling
  )
  chown -R tester:tester "$t"
  local common="ListenAddress 127.0.0.1
HostKey $t/hostkey
StrictModes no
Subsystem sftp internal-sftp"
  cat >"$t/sshd_keys" <<EOF
Port 2222
PidFile $t/sshd_keys.pid
$common
HostCertificate $t/hostkey-cert.pub
AuthorizedKeysFile $t/authorized_keys
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
AllowTcpForwarding yes
MaxAuthTries 10
EOF
  cat >"$t/sshd_questions" <<EOF
Port 2223
PidFile $t/sshd_questions.pid
$common
PubkeyAuthentication no
PasswordAuthentication no
KbdInteractiveAuthentication yes
UsePAM yes
AuthenticationMethods keyboard-interactive
EOF
  # In the foreground, so that what each connection logs reaches the file too.
  /usr/sbin/sshd -D -e -f "$t/sshd_keys" 2>"$t/sshd_keys.log" &
  /usr/sbin/sshd -D -e -f "$t/sshd_questions" 2>"$t/sshd_questions.log" &
}

start_ftp_servers() {
  local f=/tmp/ftptest
  rm -rf "$f"
  mkdir -p "$f/vsftpd" "$f/proftpd" "$f/empty"
  openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=127.0.0.1 \
    -keyout "$f/server.key" -out "$f/server.pem" 2>/dev/null
  openssl x509 -inform DER -in /src/tests/fixtures/ftps-client-ca.der -out "$f/client-ca.pem"
  local hex
  hex=$(openssl x509 -in "$f/server.pem" -outform DER | od -An -v -tx1 | tr -d ' \n')
  for port in 2121 2122 2123 2124; do echo "127.0.0.1:$port $hex"; done >"$f/known_certs"
  # Made beforehand, so that keep_server_logs can read them.
  (cd "$f" && touch vsftpd.log vsftpd-certs.log proftpd.log proftpd-tls.log && chmod 644 ./*.log)
  chown -R tester:tester "$f"
  chmod 600 "$f/server.key"
  chown root:root "$f/server.key" "$f"/*.log "$f/empty"

  local vsftpd="listen=YES
listen_address=127.0.0.1
anonymous_enable=NO
local_enable=YES
write_enable=YES
local_umask=022
local_root=$f/vsftpd
chroot_local_user=YES
allow_writeable_chroot=YES
pasv_min_port=30000
pasv_max_port=30099
connect_from_port_20=NO
ssl_enable=YES
force_local_logins_ssl=NO
force_local_data_ssl=NO
rsa_cert_file=$f/server.pem
rsa_private_key_file=$f/server.key
pam_service_name=vsftpd
secure_chroot_dir=$f/empty
seccomp_sandbox=NO
isolate=NO
isolate_network=NO
background=YES
log_ftp_protocol=YES"
  printf '%s\nlisten_port=2121\nvsftpd_log_file=%s/vsftpd.log\n' "$vsftpd" "$f" >"$f/vsftpd.conf"
  printf '%s\nlisten_port=2124\nvsftpd_log_file=%s/vsftpd-certs.log\nrequire_cert=YES\nvalidate_cert=YES\nca_certs_file=%s/client-ca.pem\nforce_local_logins_ssl=YES\n' \
    "$vsftpd" "$f" "$f" >"$f/vsftpd-certs.conf"
  vsftpd "$f/vsftpd.conf" || echo "--- vsftpd did not start"
  vsftpd "$f/vsftpd-certs.conf" || echo "--- vsftpd did not start"

  cat >"$f/proftpd.conf" <<EOF
ModulePath /usr/lib/proftpd
LoadModule mod_tls.c
ServerName "OxideMerge test"
ServerType standalone
DefaultServer on
DefaultAddress 127.0.0.1
SocketBindTight on
UseIPv6 off
Port 2122
User nobody
Group nogroup
PidFile $f/proftpd.pid
ScoreboardFile $f/proftpd.scoreboard
WtmpLog off
UseReverseDNS off
RequireValidShell off
AuthOrder mod_auth_pam.c* mod_auth_unix.c
DefaultRoot $f/proftpd
PassivePorts 30100 30199
AllowOverwrite on
Umask 022
SystemLog $f/proftpd.log
TLSEngine on
TLSLog $f/proftpd-tls.log
TLSProtocol TLSv1.2 TLSv1.3
TLSRSACertificateFile $f/server.pem
TLSRSACertificateKeyFile $f/server.key
TLSRequired off
TLSVerifyClient off
<Directory />
  AllowOverwrite on
</Directory>
<VirtualHost 127.0.0.1>
  Port 2123
  DefaultRoot $f/proftpd
  RequireValidShell off
  AuthOrder mod_auth_pam.c* mod_auth_unix.c
  AllowOverwrite on
  Umask 022
  TLSEngine on
  TLSProtocol TLSv1.2 TLSv1.3
  TLSRSACertificateFile $f/server.pem
  TLSRSACertificateKeyFile $f/server.key
  TLSOptions UseImplicitSSL
</VirtualHost>
EOF
  proftpd -c "$f/proftpd.conf" || echo "--- proftpd did not start"
}

start_proxies() {
  local p=/tmp/proxytest
  rm -rf "$p"
  mkdir -p -m 1777 "$p"
  local port method
  for port in 1080 1081; do
    method=none
    [ "$port" = 1081 ] && method=username
    cat >"$p/danted-$port.conf" <<EOF
logoutput: $p/danted-$port.log
internal: 127.0.0.1 port = $port
external: 127.0.0.1
clientmethod: none
socksmethod: $method
user.privileged: root
user.unprivileged: nobody
client pass { from: 127.0.0.0/8 to: 0.0.0.0/0 }
socks pass { from: 127.0.0.0/8 to: 0.0.0.0/0 }
EOF
    danted -D -f "$p/danted-$port.conf" -p "$p/danted-$port.pid" || echo "--- danted did not start"
  done
  for port in 8888 8889; do
    cat >"$p/tinyproxy-$port.conf" <<EOF
User nobody
Group nogroup
Port $port
Listen 127.0.0.1
Timeout 60
Allow 127.0.0.1
MaxClients 50
LogFile "$p/tinyproxy-$port.log"
PidFile "$p/tinyproxy-$port.pid"
DisableViaHeader Yes
EOF
    [ "$port" = 8888 ] && echo "BasicAuth tester $test_password" >>"$p/tinyproxy-$port.conf"
    tinyproxy -c "$p/tinyproxy-$port.conf" || echo "--- tinyproxy did not start"
  done
}

# Pins the SSH servers' host key where the tests look for known hosts, in the app's format.
pin_ssh_servers() {
  local hex
  hex=$(cut -d' ' -f2 /tmp/sshtest/hostkey.pub | base64 -d | od -An -v -tx1 | tr -d ' \n')
  mkdir -p /tmp/oxidemerge-test-config
  printf '127.0.0.1:2222 %s\n127.0.0.1:2223 %s\n' "$hex" "$hex" >/tmp/oxidemerge-test-config/known_hosts
}

# Copies the servers' logs to /out/servers.
keep_server_logs() {
  mkdir -p /out/servers
  cp /tmp/sshtest/*.log /tmp/ftptest/*.log /tmp/proxytest/*.log /out/servers/ 2>/dev/null || true
}

