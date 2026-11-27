#version 330

// ⑩ v1.5.0 沉浸模式星空穹顶 fragment：全程序化（无纹理无采样），单 pass。
// 内容（用户点单）：星星（3 层视差+闪烁）、星云、月亮（明暗界线月牙）、
// 远景恒星（金/蓝白）、吸积盘黑洞（旋转条纹+暗核+光子环）。
// 廉价 hash（fract-sin）+ 低 octave 值噪声，1080p 目标 GPU <1ms。

in vec2 texCoord0;
in vec3 vDir;

out vec4 fragColor;

const float PI = 3.14159265;

float hash21(vec2 p) {
	return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453123);
}

float noise2(vec2 p) {
	vec2 i = floor(p);
	vec2 f = fract(p);
	f = f * f * (3.0 - 2.0 * f);
	float a = hash21(i);
	float b = hash21(i + vec2(1.0, 0.0));
	float c = hash21(i + vec2(0.0, 1.0));
	float d = hash21(i + vec2(1.0, 1.0));
	return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

/** 方向 → 八面体 2D 映射（全向星场用，极点无奇变的近似） */
vec2 octaMap(vec3 d) {
	d /= (abs(d.x) + abs(d.y) + abs(d.z));
	vec2 uv = d.y >= 0.0 ? d.xz : (1.0 - abs(d.zx)) * sign(d.xz);
	return uv * 0.5 + 0.5;
}

/** 一层星星：网格抖动星点 + 按相位闪烁 */
float starLayer(vec2 uv, float density, float time, float twinkle) {
	vec2 g = uv * density;
	vec2 cell = floor(g);
	vec2 f = fract(g);
	float rnd = hash21(cell);
	// 每格最多一颗星：抖动位置 + 亮度门（疏密）
	if(rnd < 0.82) return 0.0;
	vec2 pos = vec2(hash21(cell + 7.3), hash21(cell + 19.7));
	float dist = length(f - pos);
	float brightness = 0.35 + 0.65 * hash21(cell + 3.1);
	float tw = 0.72 + 0.28 * sin(time * (1.5 + 2.0 * hash21(cell + 5.9)) + rnd * 20.0);
	return smoothstep(0.06, 0.0, dist) * brightness * mix(1.0, tw, twinkle);
}

void main() {
	vec3 d = normalize(vDir);
	float time = texCoord0.x;

	// === 深空底色：垂直渐变（天顶深蓝黑 → 地平线微光）===
	float horizon = 1.0 - abs(d.y);
	vec3 col = mix(vec3(0.004, 0.005, 0.012), vec3(0.020, 0.028, 0.055), horizon);

	// === 星云：2 octave 值噪声，蓝青染色，随视角固定 ===
	vec2 mu = octaMap(d);
	float neb = noise2(mu * 5.0) * 0.65 + noise2(mu * 11.0) * 0.35;
	neb = pow(max(neb - 0.38, 0.0), 1.4) * 1.6;
	vec3 nebCol = mix(vec3(0.10, 0.16, 0.42), vec3(0.05, 0.35, 0.38), noise2(mu * 3.0 + 4.7));
	col += nebCol * neb;

	// === 星星：3 层视差（密度/亮度递增，最近层闪烁最明显）===
	vec2 su = octaMap(d);
	float stars = starLayer(su + 13.7, 26.0, time, 0.15)
		+ starLayer(su + 91.2, 52.0, time, 0.45)
		+ starLayer(su + 45.1, 104.0, time, 0.85);
	col += vec3(0.92, 0.95, 1.0) * min(stars, 1.2);

	// === 月亮：亮盘 + 明暗界线月牙 ===
	vec3 moonDir = normalize(vec3(0.40, 0.55, -0.72));
	float md = dot(d, moonDir);
	if(md > 0.99955) {
		// 盘内：按 (d 相对 moon 的) 偏移做相位明暗
		vec3 mx = normalize(cross(moonDir, vec3(0.0, 1.0, 0.0)));
		vec3 my = cross(mx, moonDir);
		vec2 lp = vec2(dot(d, mx), dot(d, my)) * 90.0; // 盘内坐标 ~[-1,1]
		float phase = clamp(0.5 + 0.55 * lp.x, 0.0, 1.0); // 竖直明暗界线
		float surface = 0.86 + 0.14 * noise2(lp * 6.0 + 2.2); // 陨石坑质感
		float lit = mix(0.12, 1.0, smoothstep(0.0, 1.0, phase));
		col = mix(col, vec3(0.93, 0.93, 0.88) * lit * surface, smoothstep(0.99955, 0.99980, md));
	}
	// 月晕
	float halo = pow(max(md - 0.996, 0.0), 2.0) * 0.22;
	col += vec3(0.55, 0.60, 0.75) * halo;

	// === 远景恒星：金/蓝白两颗，光斑 falloff ===
	vec3 goldDir = normalize(vec3(-0.66, 0.18, 0.72));
	vec3 blueDir = normalize(vec3(0.15, 0.78, 0.60));
	float g = pow(max(dot(d, goldDir), 0.0), 3800.0);
	float b = pow(max(dot(d, blueDir), 0.0), 5200.0);
	col += vec3(1.00, 0.85, 0.55) * g * (0.8 + 0.2 * sin(time * 0.7));
	col += vec3(0.65, 0.78, 1.00) * b * (0.8 + 0.2 * sin(time * 0.9 + 2.0));
	// 恒星光斑（大而淡的辉光）
	col += vec3(0.30, 0.24, 0.12) * pow(max(dot(d, goldDir), 0.0), 220.0) * 0.5;
	col += vec3(0.12, 0.20, 0.34) * pow(max(dot(d, blueDir), 0.0), 260.0) * 0.5;

	// === 黑洞：吸积盘（旋转条纹）+ 暗核 + 光子环 ===
	vec3 bhDir = normalize(vec3(-0.62, 0.28, 0.73));
	float bd = dot(d, bhDir);
	float ang = acos(clamp(bd, -1.0, 1.0)); // 角半径
	// 盘平面基（垂直 bhDir）
	vec3 bx = normalize(cross(bhDir, vec3(0.0, 1.0, 0.0)));
	vec3 by = cross(bx, bhDir);
	// 盘带：角半径 0.05..0.16 且偏向盘平面（|sin| 权重）
	float band = smoothstep(0.045, 0.075, ang) * (1.0 - smoothstep(0.135, 0.165, ang));
	float planeW = 1.0 - smoothstep(0.25, 0.85, abs(dot(d, normalize(cross(bx, by)))));
	float disk = band * mix(0.15, 1.0, planeW);
	// 暗核（事件视界阴影）
	float core = 1.0 - smoothstep(0.028, 0.040, ang);
	// 光子环（核外细亮圈）
	float ring = smoothstep(0.032, 0.038, ang) * (1.0 - smoothstep(0.038, 0.046, ang));
	// 吸积盘热辉：旋转条纹（角向 noise 随 time 转动）+ 内亮外暗梯度
	if(disk > 0.001) {
		vec2 dp = vec2(dot(d, bx), dot(d, by));
		float angP = atan(dp.y, dp.x);
		float swirl = noise2(vec2(angP * 4.0 + time * 0.9, ang * 60.0));
		float heat = smoothstep(0.16, 0.06, ang);
		vec3 diskCol = mix(vec3(1.00, 0.62, 0.20), vec3(1.00, 0.94, 0.75), heat);
		col = mix(col, diskCol * (0.35 + 0.9 * swirl), disk * 0.9);
	}
	col = mix(col, vec3(0.0), core);            // 视界：纯黑
	col += vec3(1.0, 0.92, 0.78) * ring * 0.9;  // 光子环

	// 轻微暗角（中心视角聚焦感，沉浸）
	float vig = 1.0 - 0.22 * pow(length(octaMap(d) - 0.5) * 1.3, 2.2);
	col *= max(vig, 0.0);

	fragColor = vec4(col, 1.0);
}
