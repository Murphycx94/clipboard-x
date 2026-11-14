# 部署 ClipboardX 同步服务器（PVE + LXC + Tailscale）

服务器只保存密文：密码本的每条记录、加密后的数据密钥、加密的清单。它没有主密码，也没有秘密密钥，拿到它的全部数据也解不出任何内容。服务只监听 Tailscale 地址，不需要开放端口，也不需要 DDNS。

## 1. 创建 LXC 容器

在 PVE 里新建一个容器：

- 模板：Debian 13（`debian-13-standard`）；无特权容器，勾选"嵌套"（nesting）
- 资源：1 核、512 MB 内存、4 GB 磁盘就足够
- 网络：接在局域网网桥上，DHCP 即可（服务不监听局域网地址，这个地址只用来 SSH 进去维护）

Tailscale 需要 `/dev/net/tun`。PVE 8.1 及以上可以直接把这个设备直通给容器：在容器的"资源 › 添加 › 设备直通"里填 `/dev/net/tun`，或者在 PVE 宿主机上执行（`102` 换成容器 ID）：

```sh
pct set 102 -dev0 /dev/net/tun
```

更早的 PVE 版本：在宿主机上编辑 `/etc/pve/lxc/<容器ID>.conf`，加入下面两行，然后重启容器：

```
lxc.cgroup2.devices.allow: c 10:200 rwm
lxc.mount.entry: /dev/net/tun dev/net/tun none bind,create=file
```

国内下载官方模板很慢时，可以在宿主机上从镜像下载到 `/var/lib/vz/template/cache/`，例如 `https://mirrors.tuna.tsinghua.edu.cn/proxmox/images/system/`，再按 PVE 列出的 sha512 校验。

## 2. 在容器里安装 Tailscale

用 Tailscale 官方签名的 apt 源安装（Debian 13 代号 `trixie`）：

```sh
apt update && apt install -y curl sqlite3
curl -fsSL https://pkgs.tailscale.com/stable/debian/trixie.noarmor.gpg \
  -o /usr/share/keyrings/tailscale-archive-keyring.gpg
curl -fsSL https://pkgs.tailscale.com/stable/debian/trixie.tailscale-keyring.list \
  -o /etc/apt/sources.list.d/tailscale.list
apt update && apt install -y tailscale
tailscale up --hostname=clipboardx-sync   # 按提示在浏览器里登录，与 Mac、手机使用同一个账号
tailscale ip -4                           # 记下 100.x.y.z
```

Mac 和手机上也要安装 Tailscale 并登录同一个账号。

## 3. 安装同步服务器

从 GitHub Releases 下载 `clipboardx-sync-server-…-linux-x86_64.tar.gz`（静态编译，不依赖系统库）和同名的 `.sha256` 文件，校验后解压运行：

```sh
sha256sum -c clipboardx-sync-server-*-linux-x86_64.tar.gz.sha256
tar xzf clipboardx-sync-server-*-linux-x86_64.tar.gz
cd clipboardx-sync-server-*/
sudo ./install.sh ./clipboardx-sync-server
```

脚本会：

- 安装程序；
- 创建专用的系统用户；
- 把监听地址写成本机的 Tailscale 地址；
- 安装并启动带安全加固的 systemd 服务；
- 最后打印**服务器地址**和**管理码**。

管理码只在没有任何设备时打印，10 分钟内有效。过期后执行 `sudo systemctl restart clipboardx-sync` 会生成新的：

```sh
journalctl -u clipboardx-sync | grep 'admin code'
```

## 4. 在 ClipboardX 里开启

1. **第一台 Mac**：设置 › 云同步 › 开启，填入服务器地址（`http://100.x.y.z:8787`）和管理码。按提示抄写秘密密钥，输入主密码，完成。
2. **其他设备**：在第一台 Mac 上进入设置 › 云同步 › 添加设备，生成配对码。在新设备上选择设置 › 云同步 › 加入，填入地址和配对码，再用主密码和秘密密钥解锁。

## 维护

- **日志**：`journalctl -u clipboardx-sync -f`
- **备份**：数据库在 `/var/lib/clipboardx-sync/sync.db`。里面全是密文，可以放心备份到任何地方：

  ```sh
  sqlite3 /var/lib/clipboardx-sync/sync.db ".backup /root/clipboardx-sync-$(date +%F).db"
  ```

- **升级**：用新版本的程序再次运行 `install.sh`。已有的配置和数据会保留。
- **吊销丢失的设备**：在任一已同步的设备上，进入设置 › 云同步 › 设备，点"吊销"。
- **所有设备都丢失时**：没有设备能生成配对码，可以让服务器打印一个（10 分钟内有效）：

  ```sh
  sudo systemctl kill -s USR1 clipboardx-sync
  journalctl -u clipboardx-sync | grep 'pairing code' | tail -1
  ```

  新设备用它加入后，在解锁页点"忘记主密码？"，输入恢复助记词并设置新主密码。配对码只能换来设备令牌，令牌读到的仍然只有密文。随后记得吊销丢失的设备。
- **手动运行**（调试用）：

  ```sh
  clipboardx-sync-server --listen 100.x.y.z:8787 --data ./data
  ```
