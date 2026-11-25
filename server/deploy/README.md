# 部署 ClipboardX 同步服务器（PVE + LXC + 域名 HTTPS）

服务器只保存密文：密码本的每条记录、加密后的数据密钥、加密的清单。它没有主密码，也没有秘密密钥，拿到它的全部数据也解不出任何内容。

结构：

- 同步服务器只监听本机 `127.0.0.1:8787`。
- 前面是 Caddy，负责三件事：
  - 在域名上提供 HTTPS，证书由 Let's Encrypt 签发，通过阿里云 DNS 验证，不需要开放 80/443；
  - 把家里的公网 IPv4 和容器自己的 IPv6 及时更新到域名的 A、AAAA 记录（DDNS）；
  - 把请求转给同步服务器。
- 一个定时任务（`clipboardx-ddns-prune`，每 10 分钟一次）删掉域名上已经过期的地址。Caddy 的 DDNS 每换一次地址只会新增一条记录，不会删除旧的；不清理的话，客户端会连到已经不属于你的地址上。
- 路由器把一个端口（例如 8443）的 IPv4 转发到容器，并放行到容器 IPv6 的同一端口。

客户端填的地址形如 `https://sync.example.com:8443`。

## 准备

- **公网 IPv4**：家里宽带需要有公网 IPv4。路由器 WAN 口拿到的地址，要和外面看到的地址（如 `curl -4 ifconfig.me`）一致；如果不一致，说明是运营商的大内网，可以打电话申请公网 IP。
- **域名**：解析托管在阿里云。
- **AccessKey**：
  1. 在阿里云 RAM 控制台新建一个子账号，只授予 `AliyunDNSFullAccess`。
  2. 为它创建 AccessKey。
  3. 这把 AccessKey 只用来改解析、申请证书，不要用主账号的 AccessKey。

  `AliyunDNSFullAccess` 能改这个阿里云账号下**所有域名的所有解析记录**，不只是同步服务器这一条；阿里云解析的授权最细也只能到整个域名。账号下还有别的重要域名或记录时，可以给同步服务器单独用一个域名（甚至单独一个账号）。

## 1. 创建 LXC 容器

在 PVE 里新建一个容器：

- 模板：Debian 13（`debian-13-standard`）。选无特权容器，勾选"嵌套"（nesting）。
- 资源：1 核、512 MB 内存、4 GB 磁盘就足够。
- 网络：接在局域网网桥上。建议在路由器里给它固定一个地址，端口转发要指向这个地址。
- DNS：DDNS 查容器自己的 IPv6 时，需要 DNS 能返回 AAAA 记录。iStoreOS 等路由器常默认过滤 AAAA，所以要让容器直接用公共 DNS。在容器的 `/etc/dhcp/dhclient.conf` 末尾加一行，然后重启容器：

  ```
  supersede domain-name-servers 223.5.5.5, 223.6.6.6;
  ```

国内下载官方模板很慢时，可以在宿主机上从镜像下载到 `/var/lib/vz/template/cache/`，再按 PVE 列出的 sha512 校验。镜像地址例如 `https://mirrors.tuna.tsinghua.edu.cn/proxmox/images/system/`。

## 2. 安装

从 GitHub Releases 下载 `clipboardx-sync-server-…-linux-x86_64.tar.gz` 和同名的 `.sha256` 文件。压缩包里有：

- 同步服务器，静态编译；
- 带阿里云 DNS 模块的 Caddy；
- 安装脚本。

校验后解压，运行安装脚本：

```sh
sha256sum -c clipboardx-sync-server-*-linux-x86_64.tar.gz.sha256
tar xzf clipboardx-sync-server-*-linux-x86_64.tar.gz
cd clipboardx-sync-server-*/
sudo ./install.sh ./clipboardx-sync-server ./caddy
```

第一次运行时，脚本会生成 `/etc/clipboardx-sync/caddy.env`，然后退出。这个文件只有 root 能读。在里面填好以下内容，再运行一次脚本：

- 域名，例如 `SYNC_ZONE=example.com`、`SYNC_NAME=sync`；
- 端口，例如 `SYNC_PORT=8443`；
- AccessKey。

第二次运行时，脚本会：

- 安装同步服务器和 Caddy，各用一个专用的系统用户；
- 安装并启动带安全加固的 systemd 服务（`clipboardx-sync`、`clipboardx-caddy`）；
- 安装清理过期解析记录的定时任务（`clipboardx-ddns-prune.timer`，需要 `python3`，Debian 的标准模板自带）；
- 最后打印**服务器地址**、需要做的**端口转发**，以及**管理码**。

Caddy 启动后一两分钟内会更新 A 记录、拿到证书。过程可以这样查看：

```sh
journalctl -u clipboardx-caddy -f
```

管理码只在没有任何设备时打印，10 分钟内有效。过期后执行 `sudo systemctl restart clipboardx-sync` 生成新的，再这样查看：

```sh
journalctl -u clipboardx-sync | grep 'admin code'
```

## 3. 路由器

家用宽带一般封了入站的 80/443，所以用别的端口。OpenWrt / iStoreOS 上要配两条规则：

- **IPv4 端口转发**：在"网络 › 防火墙 › 端口转发"里，把外部 TCP `8443`（即 `SYNC_PORT`）转发到容器地址的 `8443`。
- **IPv6 放行**：IPv6 不需要转发，但路由器默认拦截从外网进来的连接。在"网络 › 防火墙 › 通信规则"里加一条：
  - 协议 TCP，来源区域 `wan`，目标区域 `lan`；
  - 目标端口 `8443`；
  - 目标地址填容器 IPv6 的后半段加掩码，例如 `::be24:11ff:fe62:3e62/::ffff:ffff:ffff:ffff`。

  这样运营商换了前缀，规则依然有效。容器 IPv6 的后半段由网卡 MAC 生成，可以用 `ip -6 addr show dev eth0 scope global` 查看。

转发配好后，在手机上关掉 Wi-Fi、用移动网络打开 `https://sync.example.com:8443/`。能连上就说明外网可以访问；返回 404 是正常的，因为服务只响应 API 路径。

## 4. 在 ClipboardX 里开启

1. **第一台设备**：设置 › 云同步 › 开启，填入服务器地址（`https://sync.example.com:8443`）和管理码。按提示抄写秘密密钥，输入主密码，完成。
2. **其他设备**：在已同步的设备上进入设置 › 云同步 › 添加设备，生成配对码。在新设备上选择设置 › 云同步 › 加入，填入地址和配对码，再用主密码和秘密密钥解锁。
3. **复制历史**（可选，0.5.0 起）：在每台想同步复制历史的设备上打开设置 › 云同步 › 同步复制历史。服务器要是 0.5.0 或更新的版本，旧版本的服务器会让这个开关下方显示"同步服务器需要升级才能同步复制历史"。

## 安全

- 服务暴露在公网上，但设备令牌只在 HTTPS 里传输，服务器上只有密文。
- 管理码和配对码只能换来设备令牌，并且：
  - 10 分钟内有效，只能用一次；
  - 连续输错会被暂时拒绝。
- 同步服务器只监听 `127.0.0.1`，外部请求只能经过 Caddy 进来。（服务单元里的 `IPAddressAllow=localhost` 是第二道保险，但在无特权 LXC 里不生效：容器无权加载它依赖的 BPF 程序。）
- 设备之间是平等的：任何一台已登记的设备都能生成配对码、吊销别的设备。设备丢失或令牌泄露时，吊销它之后再看一遍设备列表，把不认识的一并吊销。吊销之后，它生成而未用的配对码不再能登记设备；每台设备由谁添加记在日志里（`journalctl -u clipboardx-sync | grep registered`）。
- AccessKey 保存在 `/etc/clipboardx-sync/caddy.env`（0600），只有解析权限（范围见上面"准备"一节）。泄露时在 RAM 控制台禁用它，再换一把新的。

## 加固（建议）

安装脚本不会改动容器的其他设置。下面几项建议手动做一遍：

- **容器防火墙**：Caddy 是这台容器里唯一对公网的程序。压缩包里的 `nftables.conf` 限制容器能连什么、能被谁连，万一 Caddy 被攻破，入侵者不容易从这里够到家里的其他设备：
  - 出去：只能连路由器的 DHCP，以及家庭网络**之外**的 DNS（53）和 80/443 端口，外加 ping。内网地址（私有 IPv4、IPv6 的 ULA 和链路本地地址）一律拒绝。
  - 进来：同步端口、来自内网的 SSH、ping，以及 IPv6 运转所必需的邻居发现和路由通告。
  - 家里设备的**公网 IPv6 地址**和容器在同一条链路上，前缀还会变，规则按"下一跳"来认：发往外网的包下一跳是路由器的链路本地地址，直接发往目的地址的就是家里的设备，一律拒绝。
  - 还剩路由器自己**朝外的地址**（公网 IPv4、WAN 口的 IPv6）：它们算外网，容器的规则拦不住，而路由器把容器当内网设备，管理页对它是开着的。在路由器上加三条规则，拒绝容器访问路由器本身（不影响它经路由器上网）。OpenWrt / iStoreOS 上，把地址换成容器的 IPv4 和它 IPv6 地址的后半段：

    ```sh
    uci batch <<'RULES'
    set firewall.sync_dhcp=rule
    set firewall.sync_dhcp.name='sync-dhcp'
    set firewall.sync_dhcp.src='lan'
    set firewall.sync_dhcp.src_ip='192.168.1.50'
    set firewall.sync_dhcp.proto='udp'
    set firewall.sync_dhcp.dest_port='67'
    set firewall.sync_dhcp.family='ipv4'
    set firewall.sync_dhcp.target='ACCEPT'
    set firewall.sync_isolate=rule
    set firewall.sync_isolate.name='sync-isolate'
    set firewall.sync_isolate.src='lan'
    set firewall.sync_isolate.src_ip='192.168.1.50'
    set firewall.sync_isolate.proto='tcp udp'
    set firewall.sync_isolate.family='ipv4'
    set firewall.sync_isolate.target='REJECT'
    set firewall.sync_isolate_v6=rule
    set firewall.sync_isolate_v6.name='sync-isolate-v6'
    set firewall.sync_isolate_v6.src='lan'
    set firewall.sync_isolate_v6.src_ip='::be24:11ff:fe62:3e62/-64'
    set firewall.sync_isolate_v6.proto='tcp udp'
    set firewall.sync_isolate_v6.family='ipv6'
    set firewall.sync_isolate_v6.target='REJECT'
    RULES
    uci commit firewall && fw4 check && fw4 reload
    ```

    第一条放行 DHCP 续租，必须排在前面。

  规则要求容器用公共 DNS（见第 1 步）、`SYNC_PORT` 是 8443（不是的话先改文件里的端口）。先试用，两分钟后自动撤销，即使 SSH 断了也会撤销：

  ```sh
  cp nftables.conf /etc/nftables.conf
  nft -c -f /etc/nftables.conf                                  # 只检查语法
  systemd-run --on-active=120 --unit nft-undo nft flush ruleset   # 两分钟后撤销
  nft -f /etc/nftables.conf
  ```

  试用期间**另开一个终端**确认：SSH 能新连上；`curl -sI https://www.aliyun.com` 和 `curl -6 -sI https://mirrors.tuna.tsinghua.edu.cn` 能通；`curl -m 3 http://192.168.1.1`（换成你的路由器地址）被拒绝。都对了就取消撤销并让它常驻，不对就等它自己撤销：

  ```sh
  systemctl stop nft-undo.timer
  systemctl enable nftables
  ```

  之后 SSH 要走 IPv4（或内网的 IPv6 地址）。
- **系统更新**：`apt install unattended-upgrades`，安全更新会自动安装。Caddy 是随发布包一起编译的，Go 和 Caddy 的安全修复要靠重新安装新版本的压缩包才能进来，建议每一两个月升级一次（见下面的"升级"）。
- **SSH**：只允许密钥登录。在 `/etc/ssh/sshd_config.d/` 下新建一个文件，写入 `PasswordAuthentication no`，然后 `systemctl restart ssh`（已有的连接不会断）。
- **限制证书签发**：给同步服务器的域名加一条 CAA 记录，值为 `0 issue "letsencrypt.org;validationmethods=dns-01"`。这样只有能改你 DNS 的人才能为它申请到证书；临时拿到你旧 IP 地址的人不行。之后如果换用别的证书机构或验证方式，要先改这条记录，否则续期会失败。
- **PVE 和路由器**：除了同步端口，不要把别的端口（PVE 的 8006、SSH 的 22）转发到公网。

## 维护

- **日志**：`journalctl -u clipboardx-sync -f`、`journalctl -u clipboardx-caddy -f`
- **备份**：数据库在 `/var/lib/clipboardx-sync/sync.db`。里面全是密文，可以放心备份到任何地方：

  ```sh
  sqlite3 /var/lib/clipboardx-sync/sync.db ".backup /root/clipboardx-sync-$(date +%F).db"
  ```

- **升级**：用新版本的压缩包再次运行 `install.sh`。已有的配置、证书和数据都会保留。0.5.0 在数据库里加了复制历史的表，启动时自动建好；旧客户端不受影响，所以先升级服务器、再升级客户端即可。
- **解析记录清理**：每 10 分钟自动运行一次，只在删了记录、出错或判断不了时才留日志（`journalctl -u clipboardx-ddns-prune`）。想看它会删什么而不真的删除：

  ```sh
  sudo sh -c 'set -a; . /etc/clipboardx-sync/caddy.env; clipboardx-ddns-prune --dry-run'
  ```

  它只动同步服务器这个名字下的 A、AAAA 记录：地址和服务器当前地址一致的那条留下，其余删除。当前地址取不到（IPv4 要问外部服务，两家说法一致才算数），或者还没有哪条记录是当前地址时，一条也不删。别的名字、别的类型、已停用或锁定的记录一概不碰。所以不要在这个名字下手动添加别的 A、AAAA 记录：它们会被当成过期地址删掉。
- **自己构建 Caddy**（需要 Go）：

  ```sh
  server/deploy/build-caddy.sh ./caddy
  ```

- **吊销丢失的设备**：在任一已同步的设备上，进入设置 › 云同步 › 设备，点"吊销"。
- **所有设备都丢失时**：没有设备能生成配对码，可以让服务器打印一个，10 分钟内有效：

  ```sh
  sudo systemctl kill -s USR1 clipboardx-sync
  journalctl -u clipboardx-sync | grep 'pairing code' | tail -1
  ```

  新设备用它加入后，在解锁页点"忘记主密码？"，输入恢复助记词并设置新主密码。配对码只能换来设备令牌，令牌读到的仍然只有密文。随后记得吊销丢失的设备。
- **手动运行**（调试用）：

  ```sh
  clipboardx-sync-server --listen 127.0.0.1:8787 --data ./data
  ```
