#version 330

layout(std140) uniform WindowInfo {
	mat4 transform;
	float alphaBlend;
	vec4 compat;
};

uniform sampler2D Sampler0;

in vec2 texCoord;

out vec4 fragColor;

void main() {
	vec4 color = texture(Sampler0, texCoord);
	// v1.4.7 光影兼容（用户反馈"光影模式会干扰窗口的画质可见度"）：
	// Iris 着色器包的 composite 后处理（tonemap 压暗/去饱和/胶片对比）作用在
	// 世界 framebuffer 上，窗口里的应用 UI 因此发灰发暗、可读性下降。
	// 这里做轻度**预补偿**——方向与典型 tonemap 的衰减相反。参数由 Java 侧
	// ShaderCompatPolicy 数据表给出（按光影包名匹配；恒等 (1,1,1,1)=不干预）。
	// 以后要适配新光影包 = 数据表加一行，着色器不动。
	vec3 c = clamp(color.rgb, 0.0, 1.0);
	c = pow(c, vec3(1.0 / max(compat.w, 0.001)));  // 抗压暗
	c *= compat.x;                                  // 亮度
	float l = dot(c, vec3(0.2126, 0.7152, 0.0722));
	c = mix(vec3(l), c, compat.y);                  // 饱和度恢复
	c = vec3(l) + (c - vec3(l)) * compat.z;         // 对比（绕亮度轴，抗灰化）
	color.rgb = clamp(c, 0.0, 1.0);
	color.a = color.a + alphaBlend * (1 - color.a);
	if(color.a == 0.0) {
		discard;
	}
	fragColor = color;
}