package dev.evvie.waylandcraft.appsession;

import java.io.IOException;
import java.net.ConnectException;
import java.net.StandardProtocolFamily;
import java.net.UnixDomainSocketAddress;
import java.nio.channels.SocketChannel;
import java.nio.file.Files;
import java.nio.file.NoSuchFileException;
import java.nio.file.Path;

import dev.evvie.waylandcraft.appsession.AppLifecycleLogic.DisplayProbe;

/**
 * Wayland socket 连通性探测（陈旧判定的关键 IO，独立成类以便真机单测——
 * AppLifecycle 依赖 Minecraft 类图，判定层的 IO 原语必须能在纯 JVM 下被直接验证）。
 *
 * <p>三条真实路径（JDK25/Linux 实测行为，全部有单测覆盖）：
 * <ul>
 *   <li>文件不存在（会话清理过 socket）→ {@link DisplayProbe#MISSING}；
 *       JDK 非阻塞 unix connect 对 ENOENT 抛 locale 相关的 SocketException，
 *       不可靠——用 {@code Files.exists} 预检钉死这条最常见的用户现场路径；</li>
 *   <li>socket 残留在但无人监听（合成器进程死亡，1.4.1 缺陷主形态）→
 *       ConnectException → {@link DisplayProbe#REFUSED}（JDK 对 ECONNREFUSED
 *       稳定映射 ConnectException）；</li>
 *   <li>能连上（活合成器：宿主桌面 / 本会话 / 另一局游戏）→
 *       {@link DisplayProbe#CONNECTED}——红线：绝不误杀。</li>
 * </ul>
 *
 * <p>其它失败（权限、无 UNIX provider、200ms 不终结）→ UNKNOWN（调用方按
 * "不确证"处理）。注：JDK25 实测把 ENOTSOCK（路径存在但非 socket）也映射为
 * ConnectException → REFUSED——"不是能干活的合成器"语义成立，红线一侧
 * （活合成器必判 CONNECTED）不受影响。
 */
public final class AppSocketProbe {

	private AppSocketProbe() {}

	public static DisplayProbe probe(String path) {
		if (path == null) return DisplayProbe.UNKNOWN;
		UnixDomainSocketAddress addr;
		try {
			addr = UnixDomainSocketAddress.of(path);
		} catch (RuntimeException e) { // InvalidPathException 是 RuntimeException 子类
			return DisplayProbe.UNKNOWN;
		}
		try {
			if (!Files.exists(Path.of(path))) return DisplayProbe.MISSING;
		} catch (RuntimeException e) {
			return DisplayProbe.UNKNOWN; // 路径本身不可解析：不确证
		}
		// 必须是 UNIX 协议族通道——默认 open() 是 TCP provider，connect(Unix 地址)
		// 直接 UnsupportedAddressTypeException（真机验证当场抓获）。
		try (SocketChannel ch = SocketChannel.open(StandardProtocolFamily.UNIX)) {
			ch.configureBlocking(false);
			boolean connectedNow;
			try {
				connectedNow = ch.connect(addr);
			} catch (ConnectException e) {
				return DisplayProbe.REFUSED; // 文件残留在但无人监听：会话已死
			} catch (NoSuchFileException e) {
				return DisplayProbe.MISSING; // 预检与 connect 之间被清理（竞态）
			}
			if (connectedNow) return DisplayProbe.CONNECTED;
			// 极少走到的分支：非阻塞 connect 返回 false（内核排队）。200ms 完成窗，
			// 不终结即 UNKNOWN——超时绝不视为"已死"，保守优先。
			long deadline = System.currentTimeMillis() + 200;
			while (System.currentTimeMillis() < deadline) {
				try {
					if (ch.finishConnect()) return DisplayProbe.CONNECTED;
				} catch (ConnectException e) {
					return DisplayProbe.REFUSED;
				} catch (IOException e) {
					return DisplayProbe.UNKNOWN;
				}
				try {
					Thread.sleep(20);
				} catch (InterruptedException e) {
					Thread.currentThread().interrupt();
					return DisplayProbe.UNKNOWN;
				}
			}
			return DisplayProbe.UNKNOWN;
		} catch (NoSuchFileException e) {
			return DisplayProbe.MISSING;
		} catch (IOException | RuntimeException e) {
			// 权限/平台不支持（无 UNIX provider）等：不确证 → 绝不误杀。
			// 注意实测：JDK25 把 ENOTSOCK（路径存在但非 socket）映射为
			// ConnectException 走上面的 REFUSED 分支——语义仍成立（该路径不是
			// 能干活的合成器）；红线只依赖"活合成器必须 CONNECTED"这一侧。
			return DisplayProbe.UNKNOWN;
		}
	}
}
