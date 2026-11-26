package dev.evvie.waylandcraft.network;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/**
 * ACL-5 服务端报表可见性门纯函数测试（与 ServerGateLogicTest 同一模式：
 * 判定逻辑与 MC 依赖解耦，纯输入输出）。
 */
class ShareAclServerLogicTest {

	@Test
	@DisplayName("ACL 行可见性：op 全量；owner 自己窗口全量；其他人仅自己相关行")
	void aclRowVisibility() {
		assertTrue(SharedWindowServerHandler.aclRowVisible(true, false, false), "op 可见一切行");
		assertTrue(SharedWindowServerHandler.aclRowVisible(false, true, false), "owner 可见自己窗口全部行");
		assertTrue(SharedWindowServerHandler.aclRowVisible(false, false, true), "普通玩家可见自己的行");
		assertFalse(SharedWindowServerHandler.aclRowVisible(false, false, false),
			"普通玩家不得见他人授权行（防授权拓扑/用户名泄露）");
	}
}
