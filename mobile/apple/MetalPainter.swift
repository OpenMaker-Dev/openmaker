import Metal
import MetalKit

/// Draws the engine's frames (mobile/core/src/frame.rs) as egui's own painters
/// do: positions in points, colors and textures as premultiplied sRGB bytes,
/// blended without converting to linear light, so the layout looks as the
/// desktop client draws it.
final class MetalPainter {
  let device: MTLDevice
  private let queue: MTLCommandQueue
  private let pipeline: MTLRenderPipelineState
  private var textures: [UInt64: MTLTexture] = [:]
  private var samplerKeys: [UInt64: SamplerKey] = [:]
  private var samplers: [SamplerKey: MTLSamplerState] = [:]
  /// Textures the last frame released, freed when its geometry (which may
  /// still draw them, and is drawn again on a resize) is replaced.
  private var freed: [UInt64] = []
  /// The last frame's geometry, drawn again when the canvas is redrawn with
  /// no new frame (a resize).
  private var geometry: Geometry?
  /// The GPU's work for the last frame: a texture change waits for it, since
  /// that frame may still read the texture.
  private var lastCommands: MTLCommandBuffer?

  private struct Geometry {
    let vertices: MTLBuffer
    let indices: MTLBuffer
    let batches: [Batch]
    let viewport: Viewport
  }

  private struct SamplerKey: Hashable {
    let magnifyLinear: Bool
    let minifyLinear: Bool
    let wrap: TextureWrap
  }

  init?() {
    guard let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue()
    else { return nil }
    self.device = device
    self.queue = queue
    do {
      let library = try device.makeLibrary(source: Self.shaders, options: nil)
      let descriptor = MTLRenderPipelineDescriptor()
      descriptor.vertexFunction = library.makeFunction(name: "vertex_main")
      descriptor.fragmentFunction = library.makeFunction(name: "fragment_main")
      let vertex = MTLVertexDescriptor()
      vertex.attributes[0].format = .float2
      vertex.attributes[0].offset = 0
      vertex.attributes[1].format = .float2
      vertex.attributes[1].offset = 8
      vertex.attributes[2].format = .uchar4Normalized
      vertex.attributes[2].offset = 16
      vertex.layouts[0].stride = 20
      descriptor.vertexDescriptor = vertex
      let color = descriptor.colorAttachments[0]!
      color.pixelFormat = Self.pixelFormat
      color.isBlendingEnabled = true
      color.rgbBlendOperation = .add
      color.alphaBlendOperation = .add
      color.sourceRGBBlendFactor = .one
      color.destinationRGBBlendFactor = .oneMinusSourceAlpha
      color.sourceAlphaBlendFactor = .oneMinusDestinationAlpha
      color.destinationAlphaBlendFactor = .one
      pipeline = try device.makeRenderPipelineState(descriptor: descriptor)
    } catch {
      return nil
    }
  }

  /// Not sRGB: egui blends in gamma space.
  static let pixelFormat = MTLPixelFormat.bgra8Unorm

  /// Make the frame's texture changes and keep its geometry for drawing.
  func take(_ frame: Frame) {
    // egui frees a texture after the frame that last drew it, and that
    // frame's geometry is replaced now. A command buffer keeps what the GPU
    // still reads.
    for id in freed {
      textures[id] = nil
      samplerKeys[id] = nil
    }
    freed = frame.freed
    if !frame.textures.isEmpty {
      lastCommands?.waitUntilCompleted()
      for update in frame.textures { apply(update) }
    }
    geometry = makeGeometry(frame)
  }

  func draw(in view: MTKView) {
    guard let pass = view.currentRenderPassDescriptor, let drawable = view.currentDrawable,
      let commands = queue.makeCommandBuffer()
    else { return }
    pass.colorAttachments[0].loadAction = .clear
    pass.colorAttachments[0].clearColor = MTLClearColor(red: 1, green: 1, blue: 1, alpha: 1)
    guard let encoder = commands.makeRenderCommandEncoder(descriptor: pass) else { return }
    if let geometry {
      encode(geometry, into: encoder, drawable: view.drawableSize)
    }
    encoder.endEncoding()
    commands.present(drawable)
    commands.commit()
    lastCommands = commands
  }

  private func encode(
    _ geometry: Geometry, into encoder: MTLRenderCommandEncoder, drawable: CGSize
  ) {
    encoder.setRenderPipelineState(pipeline)
    encoder.setVertexBuffer(geometry.vertices, offset: 0, index: 0)
    var size = SIMD2<Float>(geometry.viewport.width, geometry.viewport.height)
    encoder.setVertexBytes(&size, length: MemoryLayout<SIMD2<Float>>.stride, index: 1)
    let width = Int(drawable.width)
    let height = Int(drawable.height)
    for batch in geometry.batches {
      guard let texture = textures[batch.texture] else { continue }
      // A frame made for the size before a rotation: clip to the drawable.
      let x = min(Int(batch.clipX), width)
      let y = min(Int(batch.clipY), height)
      let w = min(Int(batch.clipWidth), width - x)
      let h = min(Int(batch.clipHeight), height - y)
      guard w > 0, h > 0 else { continue }
      encoder.setScissorRect(MTLScissorRect(x: x, y: y, width: w, height: h))
      encoder.setFragmentTexture(texture, index: 0)
      encoder.setFragmentSamplerState(sampler(for: batch.texture), index: 0)
      encoder.drawIndexedPrimitives(
        type: .triangle, indexCount: Int(batch.indexCount), indexType: .uint32,
        indexBuffer: geometry.indices, indexBufferOffset: Int(batch.firstIndex) * 4)
    }
  }

  private func apply(_ update: TextureUpdate) {
    let width = Int(update.width)
    let height = Int(update.height)
    guard width > 0, height > 0, update.pixels.count == width * height * 4 else { return }
    let texture: MTLTexture
    if update.partial, let existing = textures[update.id] {
      texture = existing
    } else {
      let descriptor = MTLTextureDescriptor.texture2DDescriptor(
        pixelFormat: .rgba8Unorm, width: width, height: height, mipmapped: false)
      descriptor.usage = .shaderRead
      guard let made = device.makeTexture(descriptor: descriptor) else { return }
      texture = made
      textures[update.id] = made
      samplerKeys[update.id] = SamplerKey(
        magnifyLinear: update.magnifyLinear, minifyLinear: update.minifyLinear, wrap: update.wrap)
    }
    let x = update.partial ? Int(update.x) : 0
    let y = update.partial ? Int(update.y) : 0
    guard x + width <= texture.width, y + height <= texture.height else { return }
    update.pixels.withUnsafeBytes { bytes in
      guard let base = bytes.baseAddress else { return }
      texture.replace(
        region: MTLRegionMake2D(x, y, width, height), mipmapLevel: 0, withBytes: base,
        bytesPerRow: width * 4)
    }
  }

  private func sampler(for texture: UInt64) -> MTLSamplerState? {
    let key =
      samplerKeys[texture] ?? SamplerKey(magnifyLinear: true, minifyLinear: true, wrap: .clamp)
    if let made = samplers[key] { return made }
    let descriptor = MTLSamplerDescriptor()
    descriptor.magFilter = key.magnifyLinear ? .linear : .nearest
    descriptor.minFilter = key.minifyLinear ? .linear : .nearest
    let mode: MTLSamplerAddressMode =
      switch key.wrap {
      case .clamp: .clampToEdge
      case .repeat: .repeat
      case .mirror: .mirrorRepeat
      }
    descriptor.sAddressMode = mode
    descriptor.tAddressMode = mode
    let made = device.makeSamplerState(descriptor: descriptor)
    samplers[key] = made
    return made
  }

  private func makeGeometry(_ frame: Frame) -> Geometry? {
    guard !frame.vertices.isEmpty, !frame.indices.isEmpty,
      let vertices = buffer(frame.vertices), let indices = buffer(frame.indices)
    else { return nil }
    return Geometry(
      vertices: vertices, indices: indices, batches: frame.batches, viewport: frame.viewport)
  }

  private func buffer(_ data: Data) -> MTLBuffer? {
    data.withUnsafeBytes { bytes in
      bytes.baseAddress.flatMap {
        device.makeBuffer(bytes: $0, length: data.count, options: .storageModeShared)
      }
    }
  }

  private static let shaders = """
    #include <metal_stdlib>
    using namespace metal;

    struct VertexIn {
      float2 position [[attribute(0)]];
      float2 uv [[attribute(1)]];
      float4 color [[attribute(2)]];
    };

    struct VertexOut {
      float4 position [[position]];
      float2 uv;
      float4 color;
    };

    vertex VertexOut vertex_main(VertexIn in [[stage_in]], constant float2 &size [[buffer(1)]]) {
      VertexOut out;
      out.position = float4(2.0 * in.position.x / size.x - 1.0,
                            1.0 - 2.0 * in.position.y / size.y, 0.0, 1.0);
      out.uv = in.uv;
      out.color = in.color;
      return out;
    }

    fragment float4 fragment_main(VertexOut in [[stage_in]], texture2d<float> texture [[texture(0)]],
                                  sampler texture_sampler [[sampler(0)]]) {
      return in.color * texture.sample(texture_sampler, in.uv);
    }
    """
}
