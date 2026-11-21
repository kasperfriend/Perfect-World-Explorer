// Extra planes are never written by the portable tracer. The ray extent and
// the Pixel ABI of plane zero (including current reverse Z) stay the same.
const TEMPORAL_INPUT: u32 = 1u << 17u;
const TEMPORAL_RR: u32 = 1u << 18u;
const SPECULAR_MISS: f32 = 65504.;
var<private> input_hit: Pixel; // instance, vertex indices relative to mesh, barycentrics, unsupported
var<private> input_material: Pixel; // diffuse, specular, normal + roughness, specular distance
var<private> input_origin: vec3<f32>;
var<private> input_specular_leg: bool;
var<private> input_need_distance: bool;

fn temporal_begin() {
    input_hit = Pixel(NIL,0u,0.,0.,0u,0u,0u);
    input_material = Pixel(pack2x16float(vec2<f32>(0.5)),pack2x16float(vec2<f32>(0.5,0.)),0.,SPECULAR_MISS,0u,0u,0u);
    input_specular_leg = false;
    input_need_distance = false;
}

// Environment BRDF approximation (DLSS-RR Integration Guide, appendix;
// Ray Tracing Gems ch. 32). Only used for the GGX material branch. Legacy
// specular already integrates to the authored lobe weight in our tracer.
fn guide_reflectivity(f0: vec3<f32>, alpha: f32, cosine: f32) -> vec3<f32> {
    let x = abs(cosine);
    let x2 = x*x;
    let x3 = x*x2;
    let y3 = alpha*alpha*alpha;
    let bias = (0.99044-1.28514*x+alpha*(1.29678-0.755907*x)) /
        (1.+2.92338*x+59.4188*x3+alpha*(20.3225-27.0302*x+222.592*x3)+
         y3*(121.563+626.13*x+316.627*x3));
    let scale = (0.0365463+3.32707*x+alpha*(9.0632-9.04756*x)) /
        (1.+3.59685*x2-1.36772*x3+alpha*(9.04401-16.3174*x2+9.22949*x3)+
         y3*(5.56589+19.7886*x2-20.2123*x3));
    return clamp(f0*max(0.,scale)+max(0.,bias)*clamp(f0.g*50.,0.,1.),vec3<f32>(0.),vec3<f32>(1.));
}

fn temporal_primary(s: Surface, hit: Hit, ray: Ray, p: vec3<f32>, medium: u32) {
    if (frame.options.x & TEMPORAL_INPUT)==0u { return; }
    let m = (hit.instance & DYNAMIC)!=0u;
    let tri = word(m,1u)+3u*hit.primitive;
    let unsupported = (s.flags & (WATER|GLASS|GRASS_MATERIAL|TREE_MATERIAL|FOLIAGE))!=0u || medium!=0u;
    input_hit = Pixel(hit.instance,word(m,tri),bitcast<f32>(word(m,tri+1u)),
        bitcast<f32>(word(m,tri+2u)),bitcast<u32>(hit.bary.x),bitcast<u32>(hit.bary.y),u32(unsupported));
    if (frame.options.x & TEMPORAL_RR)==0u || unsupported { return; }
    var diffuse = clamp(s.color.rgb,vec3<f32>(0.),vec3<f32>(1.));
    var spec = vec3<f32>(0.);
    var roughness = 1.;
    let optical = (frame.options.x & REFLECTIONS)!=0u;
    let physical = physical_material(s) && (frame.options.x & LIGHTING)!=0u;
    if physical {
        let b = material_bsdf(s);
        diffuse = b.diffuse;
        spec = guide_reflectivity(b.f0,b.alpha,dot(s.n,-ray.dir));
        roughness = sqrt(b.alpha);
    } else if optical && frame.options.y>0u && (s.flags & EMISSIVE)==0u {
        spec = clamp(s.reflectance*s.specular,vec3<f32>(0.),vec3<f32>(1.));
        diffuse *= vec3<f32>(1.)-spec;
        if strongest(spec)>0. {
            roughness = select(0.,pow(2./(2.+max(s.power,0.)),0.25),s.power>=0.);
        }
    }
    input_need_distance = frame.options.y>0u && strongest(spec)>0.;
    input_origin = p;
    input_material = Pixel(pack2x16float(diffuse.xy),pack2x16float(vec2<f32>(diffuse.z,spec.x)),
        bitcast<f32>(pack2x16float(spec.yz)),SPECULAR_MISS,
        pack2x16float(s.n.xy),pack2x16float(vec2<f32>(s.n.z,roughness)),0u);
}

fn temporal_specular_block() { input_specular_leg = false; }
fn temporal_specular_leg(specular: bool) { input_specular_leg = specular; }
fn temporal_specular_hit(ray: Ray, hit: Hit) {
    if !input_need_distance || !input_specular_leg { return; }
    // An actual first specular leg, not primary depth or the length of a
    // diffuse path. Refract/layered paths aren't usable specular guides.
    if hit.instance!=NIL {
        let m = (hit.instance & DYNAMIC)!=0u;
        let flags = word(m,material_addr(m,hit.primitive)+1u);
        if (flags & (WATER|GLASS))!=0u { return; }
        input_material.view_depth = min(length(ray.origin+ray.dir*hit.t-input_origin),65503.);
    }
    input_need_distance = false;
}
fn temporal_end(at: u32) {
    if (frame.options.x & TEMPORAL_INPUT)==0u { return; }
    let n = frame.extent.x*frame.extent.y;
    // Where 1 spp chose a diffuse leg instead of a required specular leg,
    // use portable color rather than presenting an invented distance as RR.
    input_hit.owner |= u32(input_need_distance);
    image[at+n] = input_hit;
    if (frame.options.x & TEMPORAL_RR)!=0u { image[at+2u*n] = input_material; }
}
