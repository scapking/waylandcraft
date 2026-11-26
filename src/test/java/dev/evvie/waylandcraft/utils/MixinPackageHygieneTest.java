package dev.evvie.waylandcraft.utils;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assumptions.assumeTrue;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import java.util.stream.Stream;

import org.junit.jupiter.api.Test;

/**
 * 红线（1.4.2 事故反读）：fabric-loader 把 mixins.json 声明的 package 整包设为
 * 保留域——包内任何**未声明**的类型一旦被普通类加载器直接引用，运行时抛
 * IllegalClassLoadError（"is in a defined mixin package ... cannot be referenced
 * directly"），而且**编译与单元测试都发现不了**。1.4.2 的伴生类
 * {@code KbFirstPressHolder} 就死在这：它被 KeyboardHandlerMixin 的按键路径
 * 引用，每次按键在 KeyboardHandler.keyPress 里抛异常 → 用户游戏键盘全废
 * （"除了 win 键位其他都无法使用"）。
 *
 * 本测试静态扫描 mixin 源码包：每个顶层类型必须满足其一——
 * <ol>
 *   <li>出现在 waylandcraft.mixins.json 或 waylandcraft.client.mixins.json 的
 *       声明列表里（mixin 类与 accessor 接口都算）；</li>
 *   <li>文件头注释显式标注「已废弃」（历史占位文件，如 PauseScreenMixin）。</li>
 * </ol>
 * 违规 = 直接失败并指名文件。新增 mixin 包内文件时忘了登记 JSON，
 * 或塞了伴生工具类（正确做法：挪出保留包，例如放 utils），都会在这里被拦下。
 */
class MixinPackageHygieneTest {

	private static final Pattern TOP_LEVEL_TYPE =
		Pattern.compile("^(?:public\\s+|final\\s+|abstract\\s+|strictfp\\s+)*(class|interface|enum)\\s+(\\w+)",
			Pattern.MULTILINE);

	/** JSON 里提取引号名；只认与目录中 .java 文件同名者（避开 required/package 等键名）。 */
	private static final Pattern QUOTED = Pattern.compile("\"([A-Za-z0-9_.$]+)\"");

	@Test
	void everyTypeInMixinPackageIsDeclaredOrDeprecated() throws IOException {
		Path root = Path.of(System.getProperty("user.dir"));
		Path mixinDir = root.resolve("src/main/java/dev/evvie/waylandcraft/mixin");
		assumeTrue(Files.isDirectory(mixinDir), "非源码树工作目录（CI jar 测试等）跳过");

		List<String> jsonFiles = List.of(
			"src/main/resources/waylandcraft.mixins.json",
			"src/main/resources/waylandcraft.client.mixins.json");
		for (String rel : jsonFiles) {
			assertTrue(Files.isRegularFile(root.resolve(rel)), "缺少 mixin 配置 " + rel);
		}

		StringBuilder declared = new StringBuilder("|");
		for (String rel : jsonFiles) {
			String json = Files.readString(root.resolve(rel));
			Matcher q = QUOTED.matcher(json);
			while (q.find()) {
				declared.append(q.group(1)).append('|');
			}
		}

		try (Stream<Path> files = Files.list(mixinDir)) {
			List<Path> sources = files.filter(p -> p.getFileName().toString().endsWith(".java")).sorted().toList();
			assertFalse(sources.isEmpty(), "mixin 包扫描为空？");
			for (Path f : sources) {
				String text = Files.readString(f);
				String fileName = f.getFileName().toString().replace(".java", "");
				boolean deprecatedHeader = text.startsWith("/*") && text.indexOf("*/") < 1200 && text.contains("已废弃");
				Matcher m = TOP_LEVEL_TYPE.matcher(text);
				while (m.find()) {
					String type = m.group(2);
					boolean isDeclared = declared.toString().contains("|" + type + "|");
					assertTrue(isDeclared || deprecatedHeader,
						"mixin 保留包内的类型 " + type + "（文件 " + fileName + "）既未在 mixins.json 声明、"
							+ "也未标注「已废弃」——运行时被直接引用会抛 IllegalClassLoadError（1.4.2 事故形态）。"
							+ "请把该类型挪出 mixin 包（如 utils），或登记进 mixins.json。");
				}
			}
		}
	}
}
