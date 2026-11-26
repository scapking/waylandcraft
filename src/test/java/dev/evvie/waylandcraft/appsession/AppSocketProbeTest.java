package dev.evvie.waylandcraft.appsession;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.net.UnixDomainSocketAddress;
import java.nio.channels.ServerSocketChannel;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Map;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import dev.evvie.waylandcraft.appsession.AppLifecycleLogic.DisplayProbe;
import dev.evvie.waylandcraft.appsession.AppLifecycleLogic.StaleVerdict;

/**
 * socket 探测三态的**真实文件系统**验证（非 mock）：陈旧判定完全依赖这三条路径
 * 的实测形态——JDK 在非阻塞 unix connect 上抛什么异常、ServerSocketChannel 关闭后
 * socket 文件是否残留（正是"合成器进程死亡"的现实形态），都在这里钉死。
 *
 * <p>真机验证曾抓获两个纸面推演会写错的点：
 * ① 默认 {@code SocketChannel.open()} 是 TCP 协议族，连 Unix 地址抛
 *   UnsupportedAddressTypeException；② ENOENT 抛 locale 相关的
 *   SocketException 而非 NoSuchFileException——所以 MISSING 走存在性预检。
 *
 * <p>路径刻意放 /tmp 下短名：AF_UNIX sun_path 上限 108 字节，构建目录深路径会假阴。
 */
class AppSocketProbeTest {

	@Test
	@DisplayName("MISSING：socket 文件不存在（会话正常退出清理过）")
	void missingWhenNoFile() {
		String gone = "/tmp/wc-probe-no-such-" + System.nanoTime();
		assertEquals(DisplayProbe.MISSING, AppSocketProbe.probe(gone));
		assertEquals(DisplayProbe.UNKNOWN, AppSocketProbe.probe(null)); // 无路径不确证
	}

	@Test
	@DisplayName("CONNECTED→REFUSED：活监听可连；关闭后文件残留（合成器死亡现场）拒连")
	void connectedThenStaleFileRefuses() throws Exception {
		Path sock = Path.of("/tmp/wc-probe-" + System.nanoTime() + ".sock");
		try (ServerSocketChannel server = ServerSocketChannel.open(java.net.StandardProtocolFamily.UNIX)) {
			server.bind(UnixDomainSocketAddress.of(sock));
			assertEquals(DisplayProbe.CONNECTED, AppSocketProbe.probe(sock.toString()),
					"活合成器必须判 CONNECTED——误杀宿主实例的红线所在");

			server.close();
			// JDK 不会自动 unlink socket 文件：这正是 1.4.1 缺陷的合成器侧形态——
			// 旧会话进程死了，socket 文件还在，connect 收到 ECONNREFUSED。
			assertTrue(Files.exists(sock), "测试前提：close 后文件应残留（真实陈旧 socket 形态）");
			assertEquals(DisplayProbe.REFUSED, AppSocketProbe.probe(sock.toString()),
					"残留无监听的 socket 必须判 REFUSED——陈旧确证路径");
		} finally {
			Files.deleteIfExists(sock);
		}
	}

	@Test
	@DisplayName("非 socket 路径：JDK25 把 ENOTSOCK 也映射为 ConnectException→REFUSED；"
			+ "不变量是**永不 CONNECTED**（活合成器误判为死才是红线），REFUSED/UNKNOWN 均可")
	void nonSocketFileIsNeverConnected() throws Exception {
		Path regular = Files.createTempFile("wc-probe-plain-", ".txt");
		try {
			DisplayProbe p = AppSocketProbe.probe(regular.toString());
			assertTrue(p == DisplayProbe.REFUSED || p == DisplayProbe.UNKNOWN,
					"普通文件绝不能判 CONNECTED，实际=" + p);
		} finally {
			Files.deleteIfExists(regular);
		}
	}

	@Test
	@DisplayName("与 classify 的组合真机链（用户 1.4.1 场景复现）：wayland-N 孤儿 socket 残留 → STALE_CONFIRMED；活监听 → FOREIGN_ALIVE")
	void classifyOnRealProbeResults() throws Exception {
		// 真·wc 命名域：XDG_RUNTIME_DIR=/tmp + WAYLAND_DISPLAY=wayland-9NN，
		// socket 文件路径由 socketPath 拼出——探测与判定共用同一条真实路径。
		String name = "wayland-" + (9000 + Math.abs(System.nanoTime() % 900));
		Path sock = Path.of(AppLifecycleLogic.socketPath("/tmp", name));
		Map<String, String> orphanEnv = AppLifecycleLogic.parseEnviron(
				"WAYLAND_DISPLAY=" + name + "\0XDG_RUNTIME_DIR=/tmp\0");
		try (ServerSocketChannel server = ServerSocketChannel.open(java.net.StandardProtocolFamily.UNIX)) {
			server.bind(UnixDomainSocketAddress.of(sock));
			// 活着的会话（哪怕名字像孤儿）：CONNECTED 优先，不杀
			assertEquals(StaleVerdict.FOREIGN_ALIVE, AppLifecycleLogic.classify(
					orphanEnv, "500-wayland-1", "wayland-0",
					AppSocketProbe.probe(AppLifecycleLogic.socketPath("/tmp", name))));
			server.close();
			// 同一条目，会话死后重探：REFUSED（文件残留无监听）+ 非宿主 display → 确证陈旧
			assertEquals(StaleVerdict.STALE_CONFIRMED, AppLifecycleLogic.classify(
					orphanEnv, "500-wayland-1", "wayland-0",
					AppSocketProbe.probe(AppLifecycleLogic.socketPath("/tmp", name))));
		} finally {
			Files.deleteIfExists(sock);
		}
	}
}
