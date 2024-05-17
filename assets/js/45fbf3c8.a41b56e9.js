"use strict";(self.webpackChunkwebsite=self.webpackChunkwebsite||[]).push([[8376],{3905:(e,t,n)=>{n.r(t),n.d(t,{MDXContext:()=>d,MDXProvider:()=>m,mdx:()=>b,useMDXComponents:()=>c,withMDXComponents:()=>p});var r=n(67294);function a(e,t,n){return t in e?Object.defineProperty(e,t,{value:n,enumerable:!0,configurable:!0,writable:!0}):e[t]=n,e}function i(){return i=Object.assign||function(e){for(var t=1;t<arguments.length;t++){var n=arguments[t];for(var r in n)Object.prototype.hasOwnProperty.call(n,r)&&(e[r]=n[r])}return e},i.apply(this,arguments)}function o(e,t){var n=Object.keys(e);if(Object.getOwnPropertySymbols){var r=Object.getOwnPropertySymbols(e);t&&(r=r.filter((function(t){return Object.getOwnPropertyDescriptor(e,t).enumerable}))),n.push.apply(n,r)}return n}function s(e){for(var t=1;t<arguments.length;t++){var n=null!=arguments[t]?arguments[t]:{};t%2?o(Object(n),!0).forEach((function(t){a(e,t,n[t])})):Object.getOwnPropertyDescriptors?Object.defineProperties(e,Object.getOwnPropertyDescriptors(n)):o(Object(n)).forEach((function(t){Object.defineProperty(e,t,Object.getOwnPropertyDescriptor(n,t))}))}return e}function l(e,t){if(null==e)return{};var n,r,a=function(e,t){if(null==e)return{};var n,r,a={},i=Object.keys(e);for(r=0;r<i.length;r++)n=i[r],t.indexOf(n)>=0||(a[n]=e[n]);return a}(e,t);if(Object.getOwnPropertySymbols){var i=Object.getOwnPropertySymbols(e);for(r=0;r<i.length;r++)n=i[r],t.indexOf(n)>=0||Object.prototype.propertyIsEnumerable.call(e,n)&&(a[n]=e[n])}return a}var d=r.createContext({}),p=function(e){return function(t){var n=c(t.components);return r.createElement(e,i({},t,{components:n}))}},c=function(e){var t=r.useContext(d),n=t;return e&&(n="function"==typeof e?e(t):s(s({},t),e)),n},m=function(e){var t=c(e.components);return r.createElement(d.Provider,{value:t},e.children)},u={inlineCode:"code",wrapper:function(e){var t=e.children;return r.createElement(r.Fragment,{},t)}},f=r.forwardRef((function(e,t){var n=e.components,a=e.mdxType,i=e.originalType,o=e.parentName,d=l(e,["components","mdxType","originalType","parentName"]),p=c(n),m=a,f=p["".concat(o,".").concat(m)]||p[m]||u[m]||i;return n?r.createElement(f,s(s({ref:t},d),{},{components:n})):r.createElement(f,s({ref:t},d))}));function b(e,t){var n=arguments,a=t&&t.mdxType;if("string"==typeof e||a){var i=n.length,o=new Array(i);o[0]=f;var s={};for(var l in t)hasOwnProperty.call(t,l)&&(s[l]=t[l]);s.originalType=e,s.mdxType="string"==typeof e?e:a,o[1]=s;for(var d=2;d<i;d++)o[d]=n[d];return r.createElement.apply(null,o)}return r.createElement.apply(null,n)}f.displayName="MDXCreateElement"},53400:(e,t,n)=>{n.r(t),n.d(t,{assets:()=>l,contentTitle:()=>o,default:()=>c,frontMatter:()=>i,metadata:()=>s,toc:()=>d});var r=n(83117),a=(n(67294),n(3905));const i={},o="ZstDelta",s={unversionedId:"dev/internals/zstdelta",id:"dev/internals/zstdelta",title:"ZstDelta",description:"ZstDelta uses zstd dictionary compression to calculate",source:"@site/docs/dev/internals/zstdelta.md",sourceDirName:"dev/internals",slug:"/dev/internals/zstdelta",permalink:"/docs/dev/internals/zstdelta",draft:!1,editUrl:"https://github.com/facebookexperimental/eden/tree/main/website/docs/dev/internals/zstdelta.md",tags:[],version:"current",frontMatter:{},sidebar:"tutorialSidebar",previous:{title:"Visibility and mutation",permalink:"/docs/dev/internals/visibility-and-mutation"},next:{title:"Writing Tests",permalink:"/docs/dev/process/writing_tests"}},l={},d=[{value:"ZstDelta",id:"zstdelta-1",level:2},{value:"ZStore",id:"zstore",level:2}],p={toc:d};function c(e){let{components:t,...n}=e;return(0,a.mdx)("wrapper",(0,r.Z)({},p,n,{components:t,mdxType:"MDXLayout"}),(0,a.mdx)("h1",{id:"zstdelta"},"ZstDelta"),(0,a.mdx)("p",null,"ZstDelta uses ",(0,a.mdx)("a",{parentName:"p",href:"https://www.zstd.net"},"zstd")," dictionary compression to calculate\na compressed delta between two inputs."),(0,a.mdx)("h2",{id:"zstdelta-1"},"ZstDelta"),(0,a.mdx)("p",null,"The ",(0,a.mdx)("inlineCode",{parentName:"p"},"zstdelta")," Rust library provides ",(0,a.mdx)("inlineCode",{parentName:"p"},"diff")," and ",(0,a.mdx)("inlineCode",{parentName:"p"},"apply")," to calculate such\ncompressed deltas and restore content from deltas. You can get ",(0,a.mdx)("inlineCode",{parentName:"p"},"delta")," from\n",(0,a.mdx)("inlineCode",{parentName:"p"},"diff(a, b)"),", then restore the content of ",(0,a.mdx)("inlineCode",{parentName:"p"},"b")," using ",(0,a.mdx)("inlineCode",{parentName:"p"},"apply(a, delta)"),"."),(0,a.mdx)("p",null,"In Python, ",(0,a.mdx)("inlineCode",{parentName:"p"},"bindings.zstd")," provides access to the ",(0,a.mdx)("inlineCode",{parentName:"p"},"diff")," and ",(0,a.mdx)("inlineCode",{parentName:"p"},"apply")," functions:"),(0,a.mdx)("pre",{className:"sapling-example"},">>> import bindings, hashlib\n>>> a = b\"\".join(hashlib.sha256(str(i).encode()).digest() for i in range(1000))\n>>> len(a)\n32000\n>>> b = a[:10000] + b'x' * 10000 + a[11000:]\n>>> diff = bindings.zstd.diff(a, b)\n>>> len(diff)\n29\n>>> bindings.zstd.apply(a, diff) == b\nTrue\n"),(0,a.mdx)("h2",{id:"zstore"},"ZStore"),(0,a.mdx)("p",null,"The ",(0,a.mdx)("inlineCode",{parentName:"p"},"zstore")," Rust library provides an on-disk content store with internal\ndelta-chain management. It uses the above ",(0,a.mdx)("inlineCode",{parentName:"p"},"zstdelta")," library for delta\ncalculation and ",(0,a.mdx)("a",{parentName:"p",href:"./indexedlog"},"IndexedLog")," for on-disk storage. It is used by\n",(0,a.mdx)("a",{parentName:"p",href:"./metalog"},"MetaLog"),"."))}c.isMDXComponent=!0}}]);