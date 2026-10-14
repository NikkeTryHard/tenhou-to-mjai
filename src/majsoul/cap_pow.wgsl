// One invocation searches one decimal nonce. The host verifies every returned
// answer with SHA-256 before exposing it to the widget.
struct Input { salt: array<u32, 128>, prefix: array<u32, 8>, salt_len: u32, nibbles: u32, base: u32, count: u32 }
@group(0) @binding(0) var<storage, read> input: Input;
@group(0) @binding(1) var<storage, read_write> answer: atomic<u32>;
const K = array<u32,64>(
0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2);
fn rr(x:u32,n:u32)->u32{return (x>>n)|(x<<(32u-n));}
@compute @workgroup_size(64)
fn search(@builtin(global_invocation_id) id:vec3<u32>){
if(id.x>=input.count){return;}
let nonce=input.base+id.x;
var digits:array<u32,10>;var num=nonce;var nd=0u;
loop{digits[nd]=48u+num%10u;nd++;num/=10u;if(num==0u){break;}}
var bytes:array<u32,192>;
for(var i=0u;i<input.salt_len;i++){bytes[i]=input.salt[i];}
for(var i=0u;i<nd;i++){bytes[input.salt_len+i]=digits[nd-1u-i];}
let len=input.salt_len+nd;bytes[len]=128u;
let blocks=(len+9u+63u)/64u;let total=blocks*64u;
bytes[total-2u]=(len*8u)>>8u;bytes[total-1u]=(len*8u)&255u;
var h=array<u32,8>(0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19);
for(var block=0u;block<blocks;block++){
var w:array<u32,64>;
for(var i=0u;i<16u;i++){let p=block*64u+i*4u;w[i]=(bytes[p]<<24u)|(bytes[p+1u]<<16u)|(bytes[p+2u]<<8u)|bytes[p+3u];}
for(var i=16u;i<64u;i++){let a=w[i-15u];let b=w[i-2u];w[i]=w[i-16u]+(rr(a,7u)^rr(a,18u)^(a>>3u))+w[i-7u]+(rr(b,17u)^rr(b,19u)^(b>>10u));}
var a=h[0];var b=h[1];var c=h[2];var d=h[3];var e=h[4];var f=h[5];var g=h[6];var z=h[7];
for(var i=0u;i<64u;i++){let t1=z+(rr(e,6u)^rr(e,11u)^rr(e,25u))+((e&f)^((~e)&g))+K[i]+w[i];let t2=(rr(a,2u)^rr(a,13u)^rr(a,22u))+((a&b)^(a&c)^(b&c));z=g;g=f;f=e;e=d+t1;d=c;c=b;b=a;a=t1+t2;}
h[0]+=a;h[1]+=b;h[2]+=c;h[3]+=d;h[4]+=e;h[5]+=f;h[6]+=g;h[7]+=z;
}
for(var i=0u;i<input.nibbles;i++){let shift=28u-(i%8u)*4u;if(((h[i/8u]>>shift)&15u)!=((input.prefix[i/8u]>>shift)&15u)){return;}}
atomicMin(&answer,nonce);
}
