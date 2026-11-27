#version 330
#moj_import <minecraft:projection.glsl>
#moj_import <minecraft:dynamictransforms.glsl>

// ⑩ v1.5.1 沉浸模式近星星点层：世界固定 3D 星点的屏幕对齐 billboard。
// Position 已由 Java 折算为相机相对坐标（含无限环绕折返），UV = 面内坐标，
// Color = (色温 RGB, 亮度/尖峰强度 A)。

in vec3 Position;
in vec2 UV0;
in vec4 Color;

out vec2 texCoord0;
out vec4 vColor;

void main() {
	gl_Position = ProjMat * ModelViewMat * vec4(Position, 1.0);
	texCoord0 = UV0;
	vColor = Color;
}
