#version 330

layout(std140) uniform WindowInfo {
	mat4 transform;
	float alphaBlend;
	vec4 compat; // 光影兼容预补偿：x=亮度 y=饱和 z=对比 w=gamma（恒等=1,1,1,1）
};

in vec3 Position;
in vec2 UV0;

out vec2 texCoord;

void main() {
	gl_Position = transform * vec4(Position, 1.0);
	texCoord = UV0;
}