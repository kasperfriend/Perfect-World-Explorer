// Local surface scattering, shared by both walkers. f is in 1/sr and pdf
// is a solid-angle density. Normals/directions are unit length, and both
// directions point out of the surface. The caller clips the geometric face.
const BSDF_PI: f32 = 3.141592653589793;
struct Bsdf { diffuse: vec3<f32>, f0: vec3<f32>, alpha: f32 }
struct BsdfEval { f: vec3<f32>, pdf: f32 }
struct BsdfSample { dir: vec3<f32>, weight: vec3<f32>, pdf: f32 }
struct Optics { transmitted: vec3<f32>, fresnel: f32 }

fn fresnel_rgb(f0: vec3<f32>, cosine: f32) -> vec3<f32> {
    if max(f0.x,max(f0.y,f0.z))<=0. { return vec3<f32>(0.); }
    let x = 1.-clamp(cosine,0.,1.);
    let x2 = x*x;
    return f0 + (1.-f0)*(x2*x2*x);
}
fn dielectric(d: vec3<f32>, n: vec3<f32>, eta: f32) -> Optics {
    let cos_i = clamp(dot(-d,n),0.,1.);
    let cos_t2 = 1.-eta*eta*(1.-cos_i*cos_i);
    if cos_t2<=0. { return Optics(vec3<f32>(0.),1.); }
    let cos_t = sqrt(cos_t2);
    let rs = (eta*cos_i-cos_t)/(eta*cos_i+cos_t);
    let rp = (cos_i-eta*cos_t)/(cos_i+eta*cos_t);
    return Optics(normalize(eta*d+(eta*cos_i-cos_t)*n),clamp(0.5*(rs*rs+rp*rp),0.,1.));
}
fn bsdf_basis(n: vec3<f32>) -> mat3x3<f32> {
    let x = normalize(cross(n,select(vec3<f32>(0.,1.,0.),vec3<f32>(1.,0.,0.),abs(n.y)>0.95)));
    return mat3x3<f32>(x,cross(n,x),n);
}
fn ggx_d(c: f32, a: f32) -> f32 {
    let a2 = a*a;
    let z = max((1.-c*c)+a2*c*c,a2);
    return a2/(BSDF_PI*z*z);
}
fn ggx_g1(c: f32, a: f32) -> f32 {
    return 2.*c/(c+sqrt(a*a+(1.-a*a)*c*c));
}
fn ggx_probability(b: Bsdf, nv: f32) -> f32 {
    if max(b.f0.x,max(b.f0.y,b.f0.z))==0. { return 0.; }
    if max(b.diffuse.x,max(b.diffuse.y,b.diffuse.z))==0. { return 1.; }
    let f = fresnel_rgb(b.f0,nv);
    return clamp(dot(f,vec3<f32>(0.2126,0.7152,0.0722)),0.05,0.95);
}
fn bsdf_eval(b: Bsdf, n: vec3<f32>, wo: vec3<f32>, wi: vec3<f32>) -> BsdfEval {
    let nv = clamp(dot(n,wo),0.,1.);
    let nl = clamp(dot(n,wi),0.,1.);
    if nv<=0. || nl<=0. { return BsdfEval(vec3<f32>(0.),0.); }
    let h = normalize(wo+wi);
    let f = fresnel_rgb(b.f0,max(dot(wo,h),0.));
    let d = ggx_d(clamp(dot(n,h),0.,1.),b.alpha);
    let g = ggx_g1(nv,b.alpha);
    let pdf_g = d*g/(4.*nv); // transformed visible-normal density
    let glossy = f*(pdf_g*ggx_g1(nl,b.alpha)/nl);
    let diffuse = b.diffuse*(1.-fresnel_rgb(b.f0,nv))*(1.-fresnel_rgb(b.f0,nl))/BSDF_PI;
    return BsdfEval(diffuse+glossy,mix(nl/BSDF_PI,pdf_g,ggx_probability(b,nv)));
}
fn ggx_visible_normal(v: vec3<f32>, alpha: f32, u: vec2<f32>) -> vec3<f32> {
    // Sample the visible-normal distribution on the stretched hemisphere.
    let vh = normalize(vec3<f32>(v.xy*alpha,v.z));
    let len2 = dot(vh.xy,vh.xy);
    let x = select(vec3<f32>(1.,0.,0.),vec3<f32>(-vh.y,vh.x,0.)*inverseSqrt(max(1.e-16,len2)),len2>1.e-16);
    let y = cross(vh,x);
    let r = sqrt(u.x);
    let phi = 2.*BSDF_PI*u.y;
    let p = r*cos(phi);
    let q = mix(sqrt(max(0.,1.-p*p)),r*sin(phi),0.5*(1.+vh.z));
    let nh = p*x+q*y+sqrt(max(0.,1.-p*p-q*q))*vh;
    return normalize(vec3<f32>(alpha*nh.xy,max(nh.z,1.e-7)));
}
fn bsdf_sample(b: Bsdf, n: vec3<f32>, wo: vec3<f32>, u: vec3<f32>) -> BsdfSample {
    let basis = bsdf_basis(n);
    var wi: vec3<f32>;
    if u.x<ggx_probability(b,clamp(dot(n,wo),0.,1.)) {
        let h = basis*ggx_visible_normal(transpose(basis)*wo,b.alpha,u.yz);
        wi = reflect(-wo,h);
    } else {
        let r = sqrt(u.y);
        let phi = 2.*BSDF_PI*u.z;
        wi = basis*vec3<f32>(r*cos(phi),r*sin(phi),sqrt(1.-u.y));
    }
    let e = bsdf_eval(b,n,wo,wi);
    if e.pdf<=0. { return BsdfSample(wi,vec3<f32>(0.),0.); }
    return BsdfSample(wi,e.f*(max(dot(n,wi),0.)/e.pdf),e.pdf);
}
