# 部署 ClipboardX 同步服务器（PVE + LXC + 域名 HTTPS）

服务器只保存密文：密码本的每条记录、加密后的数据密钥、加密的清单。它没有主密码，也没有秘密密钥，拿到它的全部数据也解不出任何内容。

结构：

- 同步服务器只监听本机 `127.0.0.1:8787`。
- 前面是 Caddy，负责三件事：
  - 在域名上提供 HTTPS，证书由 Let's Encrypt 签发，通过阿里云 DNS 验证，不需要开放 80/443；
  - 把家里的公网 IPv4 及时更新到域名的 A 记录（DDNS）；
  - 把请求转给同步服务器。
- 路由器把一个端口（例如 8443）转发到容器。

客户端填的地址形如 `https://sync.example.com:8443`。

## 准备

- **公网 IPv4**：家里宽带需要有公网 IPv4。路由器 WAN 口拿到的地址，要和外面看到的地址（如 `curl -4 ifconfig.me`）一致；如果不一致，说明是运营商的大内网，可以打电话申请公网 IP。
- **域名**：解析托管在阿里云。
- **AccessKey**：
  1. 在阿里云 RAM 控制台新建一个子账号，只授予 `AliyunDNSFullAccess`。
  2. 为它创建 AccessKey。
  3. 这把 AccessKey 只用来改解析、申请证书，不要用主账号的 AccessKey。

## 1. 创建 LXC 容器

在 PVE 里新建一个容器：

- 模板：Debian 13（`debian-13-standard`）。选无特权容器，勾选"嵌套"（nesting）。
- 资源：1 核、512 MB 内存、4 GB 磁盘就足够。
- 网络：接在局域网网桥上。建议在路由器里给它固定一个地址，端口转发要指向这个地址。

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
- 最后打印**服务器地址**、需要做的**端口转发**，以及**管理码**。

Caddy 启动后一两分钟内会更新 A 记录、拿到证书。过程可以这样查看：

```sh
journalctl -u clipboardx-caddy -f
```

管理码只在没有任何设备时打印，10 分钟内有效。过期后执行 `sudo systemctl restart clipboardx-sync` 生成新的，再这样查看：

```sh
journalctl -u clipboardx-sync | grep 'admin code'
```

## 3. 路由器端口转发

在路由器上添加一条端口转发：外部 TCP `8443`（即 `SYNC_PORT`）→ 容器地址的 `8443`。

在 OpenWrt / iStoreOS 上的位置是"网络 › 防火墙 › 端口转发"。家用宽带一般封了入站的 80/443，所以用别的端口。

转发配好后，在手机上关掉 Wi-Fi、用移动网络打开 `https://sync.example.com:8443/`。能连上就说明外网可以访问；返回 404 是正常的，因为服务只响应 API 路径。

## 4. 在 ClipboardX 里开启

1. **第一台设备**：设置 › 云同步 › 开启，填入服务器地址（`https://sync.example.com:8443`）和管理码。按提示抄写秘密密钥，输入主密码，完成。
2. **其他设备**：在已同步的设备上进入设置 › 云同步 › 添加设备，生成配对码。在新设备上选择设置 › 云同步 › 加入，填入地址和配对码，再用主密码和秘密密钥解锁。

## 安全

- 服务暴露在公网上，但设备令牌只在 HTTPS 里传输，服务器上只有密文。
- 管理码和配对码只能换来设备令牌，并且：
  - 10 分钟内有效，只能用一次；
  - 连续输错会被暂时拒绝。
- 同步服务器只接受本机的连接（systemd `IPAddressAllow=localhost`）。外部请求只能经过 Caddy 进来。
- AccessKey 保存在 `/etc/clipboardx-sync/caddy.env`（0600），只能改阿里云解析。泄露时在 RAM 控制台禁用它，再换一把新的。

## 维护

- **日志**：`journalctl -u clipboardx-sync -f`、`journalctl -u clipboardx-caddy -f`
- **备份**：数据库在 `/var/lib/clipboardx-sync/sync.db`。里面全是密文，可以放心备份到任何地方：

  ```sh
  sqlite3 /var/lib/clipboardx-sync/sync.db ".backup /root/clipboardx-sync-$(date +%F).db"
  ```

- **升级**：用新版本的压缩包再次运行 `install.sh`。已有的配置、证书和数据都会保留。
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
