import Metal
import MetalKit

/// Draws the remote framebuffer as one textured quad. The texture only
/// receives the pixels that changed since the last frame.
@MainActor
final class ScreenRenderer: NSObject, MTKViewDelegate {
    let device: MTLDevice
    private let queue: MTLCommandQueue
    private let pipeline: MTLRenderPipelineState
    private let linear: MTLSamplerState
    private let nearest: MTLSamplerState
    private var texture: MTLTexture?

    /// Where the remote image sits in the view, in points.
    var imageRect = CGRect.zero
    /// The part of the texture drawn into `imageRect`, as fractions 0…1.
    var uvRect = CGRect(x: 0, y: 0, width: 1, height: 1)
    var contentScale: CGFloat = 1
    weak var session: RemoteSession?

    /// Compiled at runtime, so the build needs no Metal toolchain.
    private static let shaderSource = """
        #include <metal_stdlib>
        using namespace metal;

        struct Vertex { float2 position; float2 uv; };
        struct Fragment { float4 position [[position]]; float2 uv; };

        vertex Fragment screen_vertex(uint id [[vertex_id]], constant Vertex *vertices [[buffer(0)]]) {
            Fragment out;
            out.position = float4(vertices[id].position, 0.0, 1.0);
            out.uv = vertices[id].uv;
            return out;
        }

        fragment float4 screen_fragment(Fragment in [[stage_in]], texture2d<float> screen [[texture(0)]],
                                        sampler screenSampler [[sampler(0)]]) {
            return float4(screen.sample(screenSampler, in.uv).rgb, 1.0);
        }
        """

    init?(view: MTKView) {
        guard let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue(),
            let library = try? device.makeLibrary(source: Self.shaderSource, options: nil)
        else { return nil }
        self.device = device
        self.queue = queue
        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.vertexFunction = library.makeFunction(name: "screen_vertex")
        descriptor.fragmentFunction = library.makeFunction(name: "screen_fragment")
        descriptor.colorAttachments[0].pixelFormat = view.colorPixelFormat
        guard let pipeline = try? device.makeRenderPipelineState(descriptor: descriptor) else { return nil }
        self.pipeline = pipeline
        let sampler = MTLSamplerDescriptor()
        sampler.sAddressMode = .clampToEdge
        sampler.tAddressMode = .clampToEdge
        sampler.minFilter = .linear
        sampler.magFilter = .linear
        guard let linear = device.makeSamplerState(descriptor: sampler) else { return nil }
        sampler.minFilter = .nearest
        sampler.magFilter = .nearest
        guard let nearest = device.makeSamplerState(descriptor: sampler) else { return nil }
        self.linear = linear
        self.nearest = nearest
        super.init()
    }

    nonisolated func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {}

    nonisolated func draw(in view: MTKView) {
        MainActor.assumeIsolated {
            upload()
            render(in: view)
        }
    }

    private func upload() {
        guard let session else { return }
        _ = session.takeFrame { rgba, width, height, dirty in
            if texture == nil || texture!.width != width || texture!.height != height {
                let descriptor = MTLTextureDescriptor.texture2DDescriptor(
                    pixelFormat: .rgba8Unorm, width: width, height: height, mipmapped: false)
                descriptor.usage = .shaderRead
                descriptor.storageMode = .shared
                texture = device.makeTexture(descriptor: descriptor)
                texture?.replace(
                    region: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0, withBytes: rgba,
                    bytesPerRow: width * 4)
                return
            }
            guard dirty.width > 0, dirty.height > 0 else { return }
            texture?.replace(
                region: MTLRegionMake2D(dirty.x, dirty.y, dirty.width, dirty.height), mipmapLevel: 0,
                withBytes: rgba + (dirty.y * width + dirty.x) * 4, bytesPerRow: width * 4)
        }
    }

    private func render(in view: MTKView) {
        guard let drawable = view.currentDrawable, let pass = view.currentRenderPassDescriptor,
            let buffer = queue.makeCommandBuffer(), let encoder = buffer.makeRenderCommandEncoder(descriptor: pass)
        else { return }
        if let texture {
            let size = view.drawableSize
            let s = contentScale
            func ndc(_ x: CGFloat, _ y: CGFloat) -> (Float, Float) {
                (Float(x * s / size.width * 2 - 1), Float(1 - y * s / size.height * 2))
            }
            let (x0, y0) = ndc(imageRect.minX, imageRect.minY)
            let (x1, y1) = ndc(imageRect.maxX, imageRect.maxY)
            let (u0, v0, u1, v1) = (
                Float(uvRect.minX), Float(uvRect.minY), Float(uvRect.maxX), Float(uvRect.maxY)
            )
            var vertices: [Float] = [
                x0, y0, u0, v0,
                x1, y0, u1, v0,
                x0, y1, u0, v1,
                x1, y1, u1, v1,
            ]
            // One remote pixel covers at least two device pixels: keep edges crisp.
            let shownPixels = max(CGFloat(texture.width) * uvRect.width, 1)
            let pixelsPerRemotePixel = imageRect.width * s / shownPixels
            encoder.setRenderPipelineState(pipeline)
            encoder.setVertexBytes(&vertices, length: vertices.count * MemoryLayout<Float>.size, index: 0)
            encoder.setFragmentTexture(texture, index: 0)
            encoder.setFragmentSamplerState(pixelsPerRemotePixel >= 2 ? nearest : linear, index: 0)
            encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
        }
        encoder.endEncoding()
        buffer.present(drawable)
        buffer.commit()
    }
}
