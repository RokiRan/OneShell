//! 动态转发 (SOCKS5): 本地 SOCKS5 服务, 把 CONNECT 请求经 SSH
//! direct-tcpip 通道转出。手写最小协议实现 (无外部依赖):
//! - 仅支持 no-auth 与 CONNECT; 其他命令回 0x07 (命令不支持)
//! - 地址类型: IPv4 / 域名 / IPv6
//! - 每条连接一个任务; 转发停止时接受循环任务被 abort,
//!   已建立的桥接随会话断开自然结束 (与 OpenSSH -D 一致)
//!
//! 安全: SOCKS5 无认证, 因此 forward.rs 强制动态转发只能绑定环回地址 —
//! 绑定非环回等于开放代理, 直接拒绝。

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::ssh::{pipe_channel_tcp, SshSession};

const VER: u8 = 0x05;
const CMD_CONNECT: u8 = 0x01;
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;
const REP_SUCCESS: u8 = 0x00;
const REP_GENERAL_FAILURE: u8 = 0x01;
const REP_CMD_UNSUPPORTED: u8 = 0x07;
const REP_ATYP_UNSUPPORTED: u8 = 0x08;

/// 单条域名最大长度 (协议字段本身上限 255)
const MAX_DOMAIN_LEN: usize = 255;

/// 解析 CONNECT 请求的地址部分 (纯函数, 可单测)。
/// `data` = ATYP 之后的地址+端口字节流。返回 (host, port) 或 SOCKS5 错误码。
fn parse_socks_addr(atyp: u8, data: &[u8]) -> Result<(String, u16), u8> {
    let (host, rest) = match atyp {
        ATYP_IPV4 => {
            if data.len() < 4 {
                return Err(REP_GENERAL_FAILURE);
            }
            (
                format!("{}.{}.{}.{}", data[0], data[1], data[2], data[3]),
                &data[4..],
            )
        }
        ATYP_DOMAIN => {
            let Some(&len) = data.first() else {
                return Err(REP_GENERAL_FAILURE);
            };
            let len = len as usize;
            if data.len() < 1 + len {
                return Err(REP_GENERAL_FAILURE);
            }
            let Ok(host) = std::str::from_utf8(&data[1..1 + len]) else {
                return Err(REP_GENERAL_FAILURE);
            };
            (host.to_string(), &data[1 + len..])
        }
        ATYP_IPV6 => {
            if data.len() < 16 {
                return Err(REP_GENERAL_FAILURE);
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&data[..16]);
            (std::net::Ipv6Addr::from(octets).to_string(), &data[16..])
        }
        _ => return Err(REP_ATYP_UNSUPPORTED),
    };
    if rest.len() < 2 {
        return Err(REP_GENERAL_FAILURE);
    }
    Ok((host, u16::from_be_bytes([rest[0], rest[1]])))
}

async fn reply(tcp: &mut TcpStream, rep: u8) {
    // 绑定地址字段按协议返回全零 (客户端不使用)
    let _ = tcp
        .write_all(&[VER, rep, 0x00, ATYP_IPV4, 0, 0, 0, 0, 0, 0])
        .await;
}

/// 处理一条 SOCKS5 客户端连接: 握手 → CONNECT → 桥接进 SSH 通道。
/// 由接受循环按连接 spawn, 桥接期间占住本任务直至任一端关闭。
pub async fn handle_client(tcp: TcpStream, session: Arc<SshSession>) {
    let _ = run(tcp, session).await;
}

/// 握手 + 读取 CONNECT 目标 (不含 SSH 会话, 可回环测试)。
/// 所有错误路径都在关闭前按协议回对应错误码。
async fn read_connect_target(tcp: &mut TcpStream) -> Result<(String, u16), ()> {
    // ── 方法协商: 客户端必须提供 no-auth (0x00) ──
    let ver = tcp.read_u8().await.map_err(|_| ())?;
    let nmethods = tcp.read_u8().await.map_err(|_| ())?;
    if ver != VER || nmethods == 0 {
        return Err(());
    }
    let mut methods = vec![0u8; nmethods as usize];
    tcp.read_exact(&mut methods).await.map_err(|_| ())?;
    if !methods.contains(&0x00) {
        let _ = tcp.write_all(&[VER, 0xFF]).await; // 无可接受方法
        return Err(());
    }
    tcp.write_all(&[VER, 0x00]).await.map_err(|_| ())?;

    // ── 请求头 ──
    let mut head = [0u8; 4];
    tcp.read_exact(&mut head).await.map_err(|_| ())?;
    let [ver, cmd, _rsv, atyp] = head;
    if ver != VER {
        return Err(());
    }
    if cmd != CMD_CONNECT {
        reply(tcp, REP_CMD_UNSUPPORTED).await;
        return Err(());
    }

    // ── 按地址类型读够字节, 交纯函数解析 ──
    let data = match atyp {
        ATYP_IPV4 | ATYP_IPV6 => {
            let n = if atyp == ATYP_IPV4 { 4 } else { 16 };
            let mut buf = vec![0u8; n + 2];
            tcp.read_exact(&mut buf).await.map_err(|_| ())?;
            buf
        }
        ATYP_DOMAIN => {
            let len = tcp.read_u8().await.map_err(|_| ())? as usize;
            if len == 0 || len > MAX_DOMAIN_LEN {
                reply(tcp, REP_GENERAL_FAILURE).await;
                return Err(());
            }
            let mut buf = vec![0u8; 1 + len + 2];
            buf[0] = len as u8;
            tcp.read_exact(&mut buf[1..]).await.map_err(|_| ())?;
            buf
        }
        _ => {
            reply(tcp, REP_ATYP_UNSUPPORTED).await;
            return Err(());
        }
    };

    match parse_socks_addr(atyp, &data) {
        Ok(v) => Ok(v),
        Err(rep) => {
            reply(tcp, rep).await;
            Err(())
        }
    }
}

async fn run(mut tcp: TcpStream, session: Arc<SshSession>) -> Result<(), ()> {
    // 生产握手时限: 慢速/僵尸客户端不白占任务
    let (host, port) = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        read_connect_target(&mut tcp),
    )
    .await
    {
        Ok(r) => r?,
        Err(_) => {
            reply(&mut tcp, REP_GENERAL_FAILURE).await;
            return Err(());
        }
    };

    // ── 经 SSH 通道连接目标 ──
    let channel = {
        let handle = session.handle.lock().await;
        handle
            .channel_open_direct_tcpip(host, port as u32, "127.0.0.1", 0)
            .await
    };
    match channel {
        Ok(ch) => {
            reply(&mut tcp, REP_SUCCESS).await;
            pipe_channel_tcp(ch, tcp).await;
            Ok(())
        }
        Err(_) => {
            reply(&mut tcp, REP_GENERAL_FAILURE).await;
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ipv4() {
        let data = [93, 184, 216, 34, 0x01, 0xBB]; // 93.184.216.34:443
        assert_eq!(
            parse_socks_addr(ATYP_IPV4, &data).unwrap(),
            ("93.184.216.34".to_string(), 443)
        );
    }

    #[test]
    fn parse_domain() {
        let mut data = vec![11u8];
        data.extend_from_slice(b"example.com");
        data.extend_from_slice(&[0x00, 0x50]); // :80
        assert_eq!(
            parse_socks_addr(ATYP_DOMAIN, &data).unwrap(),
            ("example.com".to_string(), 80)
        );
    }

    #[test]
    fn parse_ipv6() {
        let mut data = vec![0u8; 15];
        data.push(1); // ::1
        data.extend_from_slice(&[0x1F, 0x90]); // :8080
        assert_eq!(
            parse_socks_addr(ATYP_IPV6, &data).unwrap(),
            ("::1".to_string(), 8080)
        );
    }

    #[test]
    fn parse_rejects_unknown_atyp() {
        assert_eq!(parse_socks_addr(0x09, &[0, 0]), Err(REP_ATYP_UNSUPPORTED));
    }

    #[test]
    fn parse_rejects_truncated() {
        assert!(parse_socks_addr(ATYP_IPV4, &[1, 2]).is_err());
        assert!(parse_socks_addr(ATYP_DOMAIN, &[20, b'a']).is_err()); // 声明 20 只给 1
        assert!(parse_socks_addr(ATYP_IPV4, &[1, 2, 3, 4, 0]).is_err()); // 缺端口
        assert!(parse_socks_addr(ATYP_IPV6, &[0u8; 10]).is_err());
    }

    #[test]
    fn parse_rejects_non_utf8_domain() {
        let data = [2u8, 0xFF, 0xFE, 0x00, 0x50];
        assert!(parse_socks_addr(ATYP_DOMAIN, &data).is_err());
    }

    #[test]
    fn parse_ignores_trailing_bytes_after_port() {
        // 解析器只消费地址+端口; 多余字节不属于本层协议, 不视为错误
        let data = [127, 0, 0, 1, 0x00, 0x50, 0xAA, 0xBB];
        assert!(parse_socks_addr(ATYP_IPV4, &data).is_ok());
    }

    // ── 回环协议测试: 真实 TCP 对端验证握手与错误回复字节 ──

    use tokio::net::TcpListener;
    use tokio::time::{timeout, Duration};

    const T: Duration = Duration::from_secs(3);

    /// 起服务任务跑 read_connect_target, 返回客户端 socket 与服务 join 句柄
    async fn loopback() -> (
        TcpStream,
        tokio::task::JoinHandle<Result<(String, u16), ()>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            read_connect_target(&mut sock).await
        });
        let client = TcpStream::connect(addr).await.unwrap();
        (client, server)
    }

    async fn read_exact_t(client: &mut TcpStream, buf: &mut [u8]) {
        timeout(T, client.read_exact(buf))
            .await
            .expect("读取超时")
            .unwrap();
    }

    async fn join_t(
        server: tokio::task::JoinHandle<Result<(String, u16), ()>>,
    ) -> Result<(String, u16), ()> {
        timeout(T, server).await.expect("服务任务超时").unwrap()
    }

    #[tokio::test]
    async fn rejects_client_without_no_auth_method() {
        let (mut client, server) = loopback().await;
        // 只提供 GSSAPI(0x01), 无 no-auth
        client.write_all(&[0x05, 0x01, 0x01]).await.unwrap();
        let mut buf = [0u8; 2];
        read_exact_t(&mut client, &mut buf).await;
        assert_eq!(buf, [0x05, 0xFF], "应回无可接受方法");
        assert!(join_t(server).await.is_err());
    }

    #[tokio::test]
    async fn rejects_unsupported_command_with_0x07() {
        let (mut client, server) = loopback().await;
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut buf = [0u8; 2];
        read_exact_t(&mut client, &mut buf).await;
        assert_eq!(buf, [0x05, 0x00]);
        // BIND (0x02) 请求
        client
            .write_all(&[0x05, 0x02, 0x00, 0x01, 127, 0, 0, 1, 0, 80])
            .await
            .unwrap();
        let mut rep = [0u8; 10];
        read_exact_t(&mut client, &mut rep).await;
        assert_eq!(rep[1], REP_CMD_UNSUPPORTED);
        assert!(join_t(server).await.is_err());
    }

    #[tokio::test]
    async fn rejects_unknown_atyp_with_0x08() {
        let (mut client, server) = loopback().await;
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut buf = [0u8; 2];
        read_exact_t(&mut client, &mut buf).await;
        client
            .write_all(&[0x05, 0x01, 0x00, 0x09, 0, 0])
            .await
            .unwrap();
        let mut rep = [0u8; 10];
        read_exact_t(&mut client, &mut rep).await;
        assert_eq!(rep[1], REP_ATYP_UNSUPPORTED);
        assert!(join_t(server).await.is_err());
    }

    #[tokio::test]
    async fn full_connect_request_parses_target() {
        let (mut client, server) = loopback().await;
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut buf = [0u8; 2];
        read_exact_t(&mut client, &mut buf).await;
        client
            .write_all(&[0x05, 0x01, 0x00, 0x01, 127, 0, 0, 1, 0x1F, 0x90])
            .await
            .unwrap();
        assert_eq!(
            join_t(server).await.unwrap(),
            ("127.0.0.1".to_string(), 8080)
        );
    }

    #[tokio::test]
    async fn truncated_request_aborts_without_reply() {
        let (mut client, server) = loopback().await;
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut buf = [0u8; 2];
        read_exact_t(&mut client, &mut buf).await;
        // 只发半截 IPv4 地址就关写端
        client
            .write_all(&[0x05, 0x01, 0x00, 0x01, 127])
            .await
            .unwrap();
        drop(client);
        assert!(join_t(server).await.is_err());
    }
}
