const vertexSource = `
attribute vec2 position;
void main() {
  gl_Position = vec4(position, 0.0, 1.0);
}
`;

const fragmentSource = `
precision highp float;
uniform vec2 resolution;
uniform float time;

const vec3 blue = vec3(0.141, 0.251, 0.659);
const vec3 violet = vec3(0.357, 0.165, 0.604);
const vec3 magenta = vec3(0.722, 0.133, 0.416);
const vec3 night = vec3(0.07, 0.06, 0.2);

float hash(vec2 p) {
  return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}

float noise(vec2 p) {
  vec2 i = floor(p);
  vec2 f = fract(p);
  vec2 u = f * f * (3.0 - 2.0 * f);
  return mix(
    mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x),
    mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x),
    u.y
  );
}

float fbm(vec2 p) {
  float value = 0.0;
  float amplitude = 0.5;
  for (int i = 0; i < 5; i++) {
    value += amplitude * noise(p);
    p = p * 2.03 + vec2(1.7, 9.2);
    amplitude *= 0.5;
  }
  return value;
}

void main() {
  vec2 uv = gl_FragCoord.xy / resolution;
  vec2 p = uv * vec2(resolution.x / resolution.y, 1.0) * 1.6;
  float t = time * 0.06;

  vec2 q = vec2(fbm(p + vec2(0.0, t)), fbm(p + vec2(5.2, 1.3) - t));
  float r = fbm(p + 3.2 * q + vec2(t * 0.7, -t * 0.4));

  vec3 color = mix(blue, violet, smoothstep(0.15, 0.75, r));
  color = mix(color, magenta, smoothstep(0.45, 0.95, r * 0.6 + uv.x * 0.55 - uv.y * 0.15));
  color = mix(color, night, smoothstep(0.55, 1.0, q.y) * 0.35);

  float band = sin((uv.x - uv.y * 0.45) * 5.0 + time * 0.35 + r * 5.0);
  color += vec3(1.0) * pow(max(band, 0.0), 24.0) * 0.16;
  color += vec3(1.0) * smoothstep(0.62, 0.95, r) * 0.07;

  float vignette = smoothstep(1.25, 0.25, length(uv - vec2(0.5, 0.42)));
  color *= mix(0.72, 1.0, vignette);

  gl_FragColor = vec4(color, 1.0);
}
`;

function compile(gl, type, source) {
  const shader = gl.createShader(type);
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    throw new Error(gl.getShaderInfoLog(shader) || "shader failed to compile");
  }
  return shader;
}

function startHero(canvas) {
  const gl = canvas.getContext("webgl", { antialias: false, alpha: false, powerPreference: "low-power" });
  if (!gl) {
    canvas.remove();
    return;
  }

  const program = gl.createProgram();
  gl.attachShader(program, compile(gl, gl.VERTEX_SHADER, vertexSource));
  gl.attachShader(program, compile(gl, gl.FRAGMENT_SHADER, fragmentSource));
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    canvas.remove();
    return;
  }
  gl.useProgram(program);

  const buffer = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
  const position = gl.getAttribLocation(program, "position");
  gl.enableVertexAttribArray(position);
  gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);

  const resolution = gl.getUniformLocation(program, "resolution");
  const time = gl.getUniformLocation(program, "time");
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
  const started = performance.now();
  let visible = true;
  let frame = 0;

  function resize() {
    const scale = Math.min(window.devicePixelRatio || 1, 1.5);
    const width = Math.max(1, Math.round(canvas.clientWidth * scale));
    const height = Math.max(1, Math.round(canvas.clientHeight * scale));
    if (canvas.width !== width || canvas.height !== height) {
      canvas.width = width;
      canvas.height = height;
      gl.viewport(0, 0, width, height);
    }
  }

  function draw(now) {
    resize();
    gl.uniform2f(resolution, canvas.width, canvas.height);
    gl.uniform1f(time, reducedMotion.matches ? 12.0 : (now - started) / 1000);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  function loop(now) {
    draw(now);
    frame = visible && !reducedMotion.matches && !document.hidden ? requestAnimationFrame(loop) : 0;
  }

  function wake() {
    if (!frame && visible && !document.hidden) {
      frame = requestAnimationFrame(loop);
    }
  }

  new IntersectionObserver(([entry]) => {
    visible = entry.isIntersecting;
    wake();
  }).observe(canvas);
  new ResizeObserver(() => draw(performance.now())).observe(canvas);
  document.addEventListener("visibilitychange", wake);
  reducedMotion.addEventListener("change", wake);
  canvas.addEventListener("webglcontextlost", (event) => {
    event.preventDefault();
    cancelAnimationFrame(frame);
    canvas.remove();
  });

  wake();
}

const canvas = document.querySelector(".stage__wallpaper");
if (canvas) {
  try {
    startHero(canvas);
  } catch {
    canvas.remove();
  }
}
